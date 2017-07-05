// FIXME: add the support of transient objects

use any_cache::{Cache, HashCache};
use notify::{Op, RawEvent, RecursiveMode, Watcher, raw_watcher};
use notify::op::WRITE;
use std::any::Any;
use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::Deref;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::sync::mpsc::channel;
use std::thread;
use std::time::{Duration, Instant};

/// Class of types that can be loaded.
pub trait Load: Sized {
  /// Arguments passed at loading.
  type Args: Clone;

  /// Static string representing the name of the type (used in filesystem caching).
  const TY_STR: &'static str;

  /// Load a resource at path `path` with arguments `args`. The `ResCache` can be used to load
  /// or declare additional resource dependencies.
  fn load<P>(path: P, cache: &mut ResCache, args: Self::Args) -> Result<LoadResult<Self>, LoadError> where P: AsRef<Path>;
}

/// Result
pub struct LoadResult<T> {
  /// The loaded object.
  res: T,
  /// The list of dependencies declared by that object.
  dependencies: Vec<PathBuf>
}

impl<T> From<T> for LoadResult<T> {
  fn from(t: T) -> Self {
    LoadResult {
      res: t,
      dependencies: Vec::new()
    }
  }
}

/// Class of types that can be reloaded.
///
/// The idea is to simply recover the arguments used in `Load::load`.
pub trait Reload: Load {
  /// Provide the arguments to use to reload a resource.
  fn reload_args(&self) -> Self::Args;
}

/// Default implementation for types which are loaded without any arguments.
impl<T> Reload for T where T: Load<Args=()> {
  fn reload_args(&self) -> Self::Args {
    ()
  }
}

/// Error that might occur while loading a resource.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LoadError {
  /// The file was not found.
  FileNotFound(PathBuf),
  /// The file wasn’t correctly parsed.
  ParseFailed(String),
  /// The file wasn’t correctly converted, even though it might have been parsed.
  ConversionFailed(String)
}

/// Resources are wrapped in this type.
#[derive(Debug)]
pub struct Res<T>(Rc<RefCell<T>>);

impl<T> Res<T> {
  /// Create a new resource.
  pub fn new(resource: T) -> Self {
    Res(Rc::new(RefCell::new(resource)))
  }

  /// Obtain a `Rc` on the wrapped resource.
  pub fn as_rc(&self) -> &Rc<RefCell<T>> {
    &self.0
  }
}

impl<T> Clone for Res<T> {
  fn clone(&self) -> Self {
    Res(self.0.clone())
  }
}

impl<T> Deref for Res<T> {
  type Target = RefCell<T>;

  fn deref(&self) -> &Self::Target {
    &self.0
  }
}

impl<T> From<Rc<RefCell<T>>> for Res<T> {
  fn from(rc: Rc<RefCell<T>>) -> Self {
    Res(rc)
  }
}

/// Time to await after a resource update to establish that it should be reloaded.
const UPDATE_AWAIT_TIME_MS: u64 = 1000;

/// Resource cache. Responsible for caching resource.
pub struct ResCache {
  // canonicalized root path of resources
  root: PathBuf,
  // contains all the typed-erased Rc<RefCell<T>>
  cache: HashCache<PathBuf>,
  // contains all metadata on resources
  metadata: HashMap<PathBuf, ResMetaData>,
  // dependencies, mapping a dependency to its observers
  dependencies: HashMap<PathBuf, PathBuf>,
  // vector of pair (path, timestamp) giving indication on resources to reload
  dirty: Arc<Mutex<Vec<(PathBuf, Instant)>>>,
  #[allow(dead_code)]
  watcher_thread: thread::JoinHandle<()>
}

/// Meta data about a resource.
struct ResMetaData {
  on_reload: Box<Fn(&mut ResCache) -> Result<(), LoadError>>,
  last_update_instant: Instant,
}

/// Error that might happen when creating a resource cache.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum ResCacheError {
  /// The root path for the resources was not found.
  RootDoesDotExit(PathBuf)
}

impl ResCache {
  /// Create a new cache.
  pub fn new<P>(root: P) -> Result<Self, ResCacheError> where P: AsRef<Path> {
    let dirty: Arc<Mutex<Vec<(PathBuf, Instant)>>> = Arc::new(Mutex::new(Vec::new()));
    let dirty_ = dirty.clone();

    let root = root.as_ref().to_owned();
    let root_ = root.clone();
    let canon_root = root.canonicalize().map_err(|_| ResCacheError::RootDoesDotExit(root_.into()))?;
    let canon_root_ = canon_root.clone();
    let (wsx, wrx) = channel();
    let mut watcher = raw_watcher(wsx).unwrap();

    let join_handle = thread::spawn(move || {
      let _ = watcher.watch(canon_root_.clone(), RecursiveMode::Recursive);

      for event in wrx.iter() {
        match event {
          RawEvent { path: Some(ref path), op: Ok(op), .. } if op | WRITE != Op::empty() => {
            dirty_.lock().unwrap().push((path.strip_prefix(&canon_root_).unwrap().to_owned(), Instant::now()));
          },
          _ => ()
        }
      }
    });

    deb!("resource cache started and listens to file changes in {}", root.display());

    Ok(ResCache {
      root: canon_root,
      cache: HashCache::new(),
      metadata: HashMap::new(),
      dependencies: HashMap::new(),
      dirty: dirty,
      watcher_thread: join_handle
    })
  }

  /// Inject a new resource in the cache.
  ///
  /// `key` is used to cache the resource and `path` is the path to where to reload the
  /// resource.
  fn inject<T>(&mut self, key: PathBuf, path: &PathBuf, resource: T, dependencies: Vec<PathBuf>, args: T::Args) -> Res<T> where T: 'static + Any + Reload {
    let res = Res(Rc::new(RefCell::new(resource)));
    let res_ = res.clone();

    let path = path.clone();
    let path_ = path.clone();
    let key_ = key.clone();

    // closure used to reload the object when needed
    let on_reload: Box<for<'a> Fn(&'a mut ResCache) -> Result<(), LoadError>> = Box::new(move |cache| {
      deb!("reloading {}", key_.display());

      match T::load(&path_, cache, args.clone()) {
        Ok(load_result) => {
          // replace the current resource with the freshly loaded one
          *res_.borrow_mut() = load_result.res;
          deb!("reloaded {}", key_.display());
          Ok(())
        },
        Err(e) => {
          warn!("{} failed to reload:\n{:#?}", key_.display(), e);
          Err(e)
        }
      }
    });

    let metadata = ResMetaData {
      on_reload: on_reload,
      last_update_instant: Instant::now(),
    };


    // cache the resource and its meta data
    self.cache.save(key.clone(), res.clone());
    self.metadata.insert(key.clone(), metadata);

    deb!("cached resource {}", key.display());

    // register the resource as an observer of its dependencies in the dependencies graph
    for dep_key in dependencies {
      self.dependencies.insert(dep_key, path.clone());
    }

    res
  }

  /// Get a resource from the cache and return an error if loading failed.
  fn get_<T>(&mut self, key: &str, args: T::Args) -> Result<Res<T>, LoadError> where T: 'static + Any + Reload {
    let key = PathBuf::from(format!("{}/{}", T::TY_STR, key));
    let path = self.root.join(&key);

    match self.cache.get::<Res<T>>(&path).cloned() {
      Some(resource) => {
        deb!("cache hit for {}", key.display());
        Ok(resource)
      },
      None => {
        deb!("cache miss for {}", key.display());

        // specific loading
        if path.exists() {
          info!("loading {}", key.display());
          let load_result = T::load(&path, self, args.clone())?;
          Ok(self.inject(key, &path, load_result.res, load_result.dependencies, args))
        } else {
          Err(LoadError::FileNotFound(key))
        }
      }
    }
  }

  /// Get a resource from the cache for the given key.
  pub fn get<T>(&mut self, key: &str, args: T::Args) -> Option<Res<T>> where T: 'static + Any + Reload {
    deb!("getting {}", key);

    match self.get_(key, args) {
      Ok(resource) => Some(resource),
      Err(e) => {
        err!("cannot get {} because:\n{:#?}", key, e);
        None
      }
    }
  }

  /// Get a resource from the cache for the given key. If it fails, a proxy version is used, which
  /// will get replaced by the resource once it’s available.
  pub fn get_proxied<T, P>(&mut self, key: &str, args: T::Args, proxy: P) -> Result<Res<T>, LoadError>
      where T: 'static + Any + Reload,
            P: FnOnce() -> T {
    match self.get_::<T>(key, args.clone()) {
      Ok(resource) => Ok(resource),
      Err(e) => {
        let key = PathBuf::from(format!("{}/{}", T::TY_STR, key));
        let path = self.root.join(&key);

        warn!("proxied {} because:\n{:#?}", key.display(), e);

        // FIXME: we set the dependencies to none here, which is silly; find a better design
        Ok(self.inject(key, &path, proxy(), Vec::new(), args))
      }
    }
  }

  /// Synchronize the cache by updating the resources that ought to.
  pub fn sync(&mut self) {
    let dirty = self.dirty.clone();
    let mut dirty_ = dirty.lock().unwrap();

    for &(ref path, ref instant) in dirty_.iter() {
      if let Some(mut metadata) = self.metadata.remove(path) {
        if instant.duration_since(metadata.last_update_instant) >= Duration::from_millis(UPDATE_AWAIT_TIME_MS) {
          if (metadata.on_reload)(self).is_ok() {
            // if we have successfully reloaded the resource, notify the observers that this
            // dependency has changed
            for dep in self.dependencies.get(path.as_path()).cloned() {
              if let Some(mut obs_metadata) = self.metadata.remove(dep.as_path()) {
                (obs_metadata.on_reload)(self);
                self.metadata.insert(dep, obs_metadata);
              }
            }
          }
        }

        metadata.last_update_instant = *instant;
        self.metadata.insert(path.clone(), metadata);
      }
    }

    dirty_.clear();
  }
}
