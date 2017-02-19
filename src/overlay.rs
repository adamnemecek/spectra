use luminance::{Dim2, Equation, Factor, Flat, Framebuffer, Mode, Pipe, Pipeline, RenderCommand, ShadingCommand, Tess, TessRender, TessVertices, Uniform, Unit, Vertex, VertexFormat};
use std::cell::RefCell;

use resource::Res;
use scene::Scene;
use shader::Program;
use text::TextTexture;
use texture::{RGBA32F, Texture};

/// Vertex used in overlay’s objects. Position coordinates are in *window space*.
#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Vert {
  /// Position.
  pub pos: [f32; 3],
  /// Color.
  pub color: [f32; 4]
}

impl Vert {
  pub fn new(pos: [f32; 3], color: [f32; 4]) -> Self {
    Vert {
      pos: pos,
      color: color
    }
  }

  /// Convert the vertex’ position to *clip space*.
  ///
  /// > Note: the depth coordinate is not affected.
  fn to_clip_space(&self, converter: &UnitConverter) -> Self {
    let converted = converter.from_window(self.pos[0], self.pos[1]);
    let pos_ = [converted[0], converted[1], self.pos[2]];

    Vert {
      pos: pos_,
      color: self.color
    }
  }
}

impl Vertex for Vert {
  fn vertex_format() -> VertexFormat {
    <([f32; 3], [f32; 4]) as Vertex>::vertex_format()
  }
}

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Triangle(pub Vert, pub Vert, pub Vert);

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Quad(pub Vert, pub Vert, pub Vert, pub Vert);

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Disc {
  center: Vert,
  radius: f32
}

impl Disc {
  pub fn new(center: Vert, radius: f32) -> Self {
    Disc {
      center: center,
      radius: radius
    }
  }

  fn to_clip_space(&self, converter: &UnitConverter) -> Self {
    Disc {
      center: self.center.to_clip_space(converter),
      radius: self.radius
    }
  }
}

impl Vertex for Disc {
  fn vertex_format() -> VertexFormat {
    <(Vert, f32) as Vertex>::vertex_format()
  }
}

pub type Texture2D = Texture<Flat, Dim2, RGBA32F>;

pub struct Text<'a> {
  text_texture: &'a TextTexture,
  left_upper: Vert
}

impl<'a> Text<'a> {
  pub fn new(text_texture: &'a TextTexture, left_upper: Vert) -> Self {
    Text {
      text_texture: text_texture,
      left_upper: left_upper
    }
  }
}

const DISC_SCREEN_RATIO: &'static Uniform<f32> = &Uniform::new(0);

const TEXT_SAMPLER: &'static Uniform<Unit> = &Uniform::new(0);
const TEXT_POS: &'static Uniform<[f32; 3]> = &Uniform::new(1);
const TEXT_SIZE: &'static Uniform<[f32; 2]> = &Uniform::new(2);
const TEXT_SCALE: &'static Uniform<f32> = &Uniform::new(3);
const TEXT_COLOR: &'static Uniform<[f32; 4]> = &Uniform::new(4);

/// A renderer responsible of rendering shapes.
pub struct Renderer {
  framebuffer: Framebuffer<Flat, Dim2, Texture2D, ()>,
  ratio: f32,
  tri_program: Res<Program>,
  tris: RefCell<Tess>,
  tri_vert_nb: RefCell<usize>,
  quads: RefCell<Tess>,
  quad_vert_nb: RefCell<usize>,
  disc_program: Res<Program>,
  discs: RefCell<Tess>,
  disc_vert_nb: RefCell<usize>,
  text_program: Res<Program>,
  text_quad: Tess,
  unit_converter: UnitConverter,
}

impl Renderer {
  pub fn new(w: u32, h: u32, max_tris: usize, max_quads: usize, max_discs: usize, scene: &mut Scene) -> Self {
    let fb = Framebuffer::new((w, h), 0).unwrap();

    let tri_program = scene.get("spectra/overlay/triangle.glsl", vec![]).unwrap();
    let tris = Tess::new(Mode::Triangle, TessVertices::Reserve::<Vert>(max_tris * 3), None);

    let quads = Tess::new(Mode::TriangleStrip, TessVertices::Reserve::<Vert>(max_quads * 4), None);

    let disc_program = scene.get("spectra/overlay/disc.glsl", vec![DISC_SCREEN_RATIO.sem("ratio")]).unwrap();
    let discs = Tess::new(Mode::Point, TessVertices::Reserve::<Disc>(max_discs), None);

    let text_program = scene.get("spectra/overlay/text.glsl", vec![
      TEXT_SAMPLER.sem("text_texture"),
      TEXT_POS.sem("pos"),
      TEXT_SIZE.sem("size"),
      TEXT_SCALE.sem("scale"),
      TEXT_COLOR.sem("color")
    ]).unwrap();

    let text_quad = Tess::attributeless(Mode::TriangleStrip, 4);

    Renderer {
      framebuffer: fb,
      ratio: w as f32 / h as f32,
      tri_program: tri_program,
      tris: RefCell::new(tris),
      tri_vert_nb: RefCell::new(0),
      quads: RefCell::new(quads),
      quad_vert_nb: RefCell::new(0),
      disc_program: disc_program,
      discs: RefCell::new(discs),
      disc_vert_nb: RefCell::new(0),
      text_program: text_program,
      text_quad: text_quad,
      unit_converter: UnitConverter::new(w, h)
    }
  }

  // Dispatch the supported shape.
  fn dispatch(&self, tris: &mut Tess, quads: &mut Tess, discs: &mut Tess, input: &RenderInput) {
    let mut tris = tris.as_slice_mut().unwrap();
    let mut tri_i = 0;
    let mut quads = quads.as_slice_mut().unwrap();
    let mut quad_i = 0;
    let mut discs = discs.as_slice_mut().unwrap();
    let mut disc_i = 0;

    for &Triangle(a, b, c) in input.triangles {
      let abc = [a, b, c];

      for &v in &abc {
        tris[tri_i] = v.to_clip_space(&self.unit_converter);
        tri_i += 1;
      }
    }

    for &Quad(a, b, c, d) in input.quads {
      let abcd = [a, b, c, d];

      for &v in &abcd {
        quads[quad_i] = v.to_clip_space(&self.unit_converter);
        quad_i += 1;
      }
    }

    for disc in input.discs {
      discs[disc_i] = disc.to_clip_space(&self.unit_converter);
      disc_i += 1;
    }

    *self.tri_vert_nb.borrow_mut() = tri_i;
    *self.quad_vert_nb.borrow_mut() = quad_i;
    *self.disc_vert_nb.borrow_mut() = disc_i;
  }

  pub fn render(&self, input: RenderInput) -> &Texture2D {
    self.dispatch(&mut self.tris.borrow_mut(), &mut self.quads.borrow_mut(), &mut self.discs.borrow_mut(), &input);

    let tris_ref = self.tris.borrow();
    let tris = TessRender::one_sub(&tris_ref, *self.tri_vert_nb.borrow());
    let discs_ref = self.discs.borrow();
    let discs = TessRender::one_sub(&discs_ref, *self.disc_vert_nb.borrow());
    let text_quad = TessRender::one_whole(&self.text_quad);

    // FIXME: no alloc?
    let text_uniforms: Vec<_> = input.texts.map(|(texts, text_scale)| texts.iter().map(|text| {
      let (tex_w, tex_h) = text.text_texture.size();
      
      [
        TEXT_SAMPLER.alter(Unit::new(0)),
        TEXT_POS.alter(text.left_upper.pos),
        TEXT_SIZE.alter(self.unit_converter.from_window(tex_w as f32, tex_h as f32)),
        TEXT_SCALE.alter(text_scale),
        TEXT_COLOR.alter(text.left_upper.color),
      ]
    }).collect()).unwrap_or(Vec::new());

    let text_textures: Vec<_> = input.texts.map(|(texts, _)| texts.iter().map(|text| [&***text.text_texture]).collect()).unwrap_or(Vec::new());

    let text_render_cmds: Vec<_> = input.texts.iter().enumerate().map(|(i, _)| {
        let blending = (Equation::Additive, Factor::One, Factor::SrcAlphaComplement);
        Pipe::empty()
          .uniforms(&text_uniforms[i])
          .textures(&text_textures[i])
          .unwrap(RenderCommand::new(Some(blending), true, vec![
            Pipe::new(text_quad.clone())
          ]))
      }).collect();

    let disc_uniforms = [
      DISC_SCREEN_RATIO.alter(self.ratio)
    ];

    Pipeline::new(&self.framebuffer, [0., 0., 0., 0.], &[], &[], vec![
      // render triangles
      Pipe::new(ShadingCommand::new(&self.tri_program.borrow(), vec![
        Pipe::new(RenderCommand::new(None, true, vec![
          Pipe::new(tris)
        ]))
      ])),
      // render discs
      Pipe::empty()
        .uniforms(&disc_uniforms)
        .unwrap(ShadingCommand::new(&self.disc_program.borrow(), vec![
          Pipe::new(RenderCommand::new(None, true, vec![
            Pipe::new(discs)
          ]))
        ])),
      // render texts
      Pipe::new(ShadingCommand::new(&self.text_program.borrow(), text_render_cmds))
    ]).run();

    &self.framebuffer.color_slot
  }
}

#[derive(Clone)]
pub struct RenderInput<'a, 'b> where 'b: 'a {
  triangles: &'a [Triangle],
  quads: &'a [Quad],
  discs: &'a [Disc],
  texts: Option<(&'a [Text<'b>], f32)>
}

impl<'a, 'b> RenderInput<'a, 'b> where 'b: 'a {
  pub fn new() -> Self {
    RenderInput {
      triangles: &[],
      quads: &[],
      discs: &[],
      texts: None,
    }
  }

  pub fn triangles(self, triangles: &'a [Triangle]) -> Self {
    RenderInput {
      triangles: triangles,
      ..self
    }
  }

  pub fn quads(self, quads: &'a [Quad]) -> Self {
    RenderInput {
      quads: quads,
      ..self
    }
  }

  pub fn discs(self, discs: &'a [Disc]) -> Self {
    RenderInput {
      discs: discs,
      ..self
    }
  }

  pub fn texts(self, texts: &'a [Text<'b>], scale: f32) -> Self {
    RenderInput {
      texts: Some((texts, scale.max(0.))),
      ..self
    }
  }
}

/// A *unit converter* is used to position stuff in overlay in a universal and screen independent
/// way.
///
/// The idea is that you provide should handle *window space* coordinates only. This type will then
/// help compute normalized coordinates the renderer can understand.
///
/// For the record, *window space* coordinates are defined as below – given a viewport size of
/// *w × h*:
///
/// ^ y = h
/// |
/// |
/// |    x = w
/// O------>
///
/// The formula used to convert from *window space* to what we call *clip space* is:
///
/// > x' = 2x / w - 1
/// > y' = 2y / h - 1
#[derive(Copy, Clone, Debug, PartialEq)]
struct UnitConverter {
  twice_rw: f32,
  twice_rh: f32
}

impl UnitConverter {
  /// Build a unit converter by giving it the size of the viewport.
  pub fn new(w: u32, h: u32) -> Self {
    UnitConverter {
      twice_rw: 2. / (w as f32),
      twice_rh: 2. / (h as f32)
    }
  }

  /// Convert from *window space* coordinates.
  pub fn from_window(&self, x: f32, y: f32) -> [f32; 2] {
    let x_ = x * self.twice_rw - 1.;
    assert!(x_ >= -1. && x_ <= 1., "x={}", x_);

    let y_ = y * self.twice_rh;
    assert!(y_ >= -1. && y_ <= 1., "y={}", y_);

    [x_, y_]
  }
}
