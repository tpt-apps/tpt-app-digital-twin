//! Spike: draw a custom OpenGL scene inside an egui window.
//!
//! Question answered: can the Phase 7 3D viewport be a raw glow scene, drawn
//! from an egui paint callback, with egui still handling the panels around it?
//!
//! The scene is a single triangle, with a perspective matrix and a rotation
//! that changes every frame. Shaders, buffers and the MVP uniform are all
//! hand-written, so this shows the GPU path works. It does not provide an
//! orbit camera, picking, or meshes from `tpt-vertex`.

use std::sync::Arc;

use eframe::egui;
use eframe::glow::{self, HasContext};
use egui::mutex::Mutex;

const VERTEX: &str = r#"#version 330 core
layout (location = 0) in vec3 a_pos;
layout (location = 1) in vec3 a_color;
uniform mat4 u_mvp;
out vec3 v_color;
void main() {
    gl_Position = u_mvp * vec4(a_pos, 1.0);
    v_color = a_color;
}
"#;

const FRAGMENT: &str = r#"#version 330 core
in vec3 v_color;
out vec4 frag_color;
void main() {
    frag_color = vec4(v_color, 1.0);
}
"#;

/// GL handles for the scene. They are plain IDs, so they are cheap to copy.
#[derive(Clone, Copy)]
struct Scene {
    program: glow::Program,
    vao: glow::VertexArray,
    vbo: glow::Buffer,
    mvp_loc: Option<glow::UniformLocation>,
}

struct ViewportApp {
    scene: Arc<Mutex<Option<Scene>>>,
    angle: f32,
}

impl ViewportApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let scene = cc
            .gl
            .as_ref()
            .map(|gl| unsafe { build_scene(gl) })
            .map(|s| Arc::new(Mutex::new(Some(s))))
            .unwrap_or_else(|| Arc::new(Mutex::new(None)));
        Self { scene, angle: 0.0 }
    }
}

/// Compile the shader program and upload the triangle.
///
/// # Safety
/// Must be called on the thread that owns the GL context.
unsafe fn build_scene(gl: &glow::Context) -> Scene {
    unsafe {
        let program = gl.create_program().expect("create program");
        let shaders = [
            (glow::VERTEX_SHADER, VERTEX),
            (glow::FRAGMENT_SHADER, FRAGMENT),
        ]
        .map(|(kind, src)| {
            let shader = gl.create_shader(kind).expect("create shader");
            gl.shader_source(shader, src);
            gl.compile_shader(shader);
            assert!(
                gl.get_shader_compile_status(shader),
                "shader error: {}",
                gl.get_shader_info_log(shader)
            );
            gl.attach_shader(program, shader);
            shader
        });
        gl.link_program(program);
        assert!(
            gl.get_program_link_status(program),
            "link error: {}",
            gl.get_program_info_log(program)
        );
        for shader in shaders {
            gl.detach_shader(program, shader);
            gl.delete_shader(shader);
        }

        // Interleaved position (xyz) and colour (rgb), one vertex per row.
        #[rustfmt::skip]
        let vertices: [f32; 18] = [
            // position         colour
            -0.6, -0.4, 0.0,    1.0, 0.2, 0.2,
             0.6, -0.4, 0.0,    0.2, 1.0, 0.2,
             0.0,  0.6, 0.0,    0.2, 0.4, 1.0,
        ];
        let vao = gl.create_vertex_array().expect("create vao");
        let vbo = gl.create_buffer().expect("create vbo");
        gl.bind_vertex_array(Some(vao));
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(vbo));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck_cast(&vertices),
            glow::STATIC_DRAW,
        );
        let stride = 6 * std::mem::size_of::<f32>() as i32;
        gl.vertex_attrib_pointer_f32(0, 3, glow::FLOAT, false, stride, 0);
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(
            1,
            3,
            glow::FLOAT,
            false,
            stride,
            3 * std::mem::size_of::<f32>() as i32,
        );
        gl.enable_vertex_attrib_array(1);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);

        let mvp_loc = gl.get_uniform_location(program, "u_mvp");
        Scene {
            program,
            vao,
            vbo,
            mvp_loc,
        }
    }
}

/// View a `[f32]` as bytes without a dependency.
fn bytemuck_cast(data: &[f32]) -> &[u8] {
    // SAFETY: f32 has no padding and any bit pattern is a valid u8 view.
    unsafe {
        std::slice::from_raw_parts(data.as_ptr().cast::<u8>(), std::mem::size_of_val(data))
    }
}

/// Column-major 4x4 matrix product `a * b`.
fn mat_mul(a: [f32; 16], b: [f32; 16]) -> [f32; 16] {
    let mut out = [0.0; 16];
    for col in 0..4 {
        for row in 0..4 {
            out[col * 4 + row] = (0..4).map(|k| a[k * 4 + row] * b[col * 4 + k]).sum();
        }
    }
    out
}

/// Rotation around Y, then a perspective projection, column-major.
fn mvp(angle: f32, aspect: f32) -> [f32; 16] {
    let (s, c) = angle.sin_cos();
    let rot_y = [
        c, 0.0, -s, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        s, 0.0, c, 0.0, //
        0.0, 0.0, 0.0, 1.0,
    ];
    let f = 1.0 / (0.5_f32).tan(); // 60 degree field of view, 1/tan(fov/2)
    let (near, far) = (0.1_f32, 10.0_f32);
    let proj = [
        f / aspect, 0.0, 0.0, 0.0, //
        0.0, f, 0.0, 0.0, //
        0.0, 0.0, (far + near) / (near - far), -1.0, //
        0.0, 0.0, (2.0 * far * near) / (near - far), 0.0,
    ];
    // Move the triangle 2 units in front of the camera.
    let view = [
        1.0, 0.0, 0.0, 0.0, //
        0.0, 1.0, 0.0, 0.0, //
        0.0, 0.0, 1.0, 0.0, //
        0.0, 0.0, -2.0, 1.0,
    ];
    mat_mul(proj, mat_mul(view, rot_y))
}

impl eframe::App for ViewportApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.angle += ctx.input(|i| i.stable_dt).min(0.1);
        ctx.request_repaint();

        egui::SidePanel::left("panel").show(ctx, |ui| {
            ui.heading("Viewport spike");
            ui.label("Panels are egui. The centre is a raw glow scene.");
            ui.label(format!("angle: {:.2} rad", self.angle));
            if self.scene.lock().is_none() {
                ui.colored_label(egui::Color32::RED, "no GL context: scene not drawn");
            }
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            let rect = ui.available_rect_before_wrap();
            let aspect = (rect.width() / rect.height().max(1.0)).max(0.01);
            let angle = self.angle;
            let scene = Arc::clone(&self.scene);

            let callback = egui_glow::CallbackFn::new(move |info, painter| {
                let Some(scene) = *scene.lock() else { return };
                let gl = painter.gl();
                let vp = info.viewport_in_pixels();
                let (x, y, w, h) = (vp.left_px, vp.from_bottom_px, vp.width_px, vp.height_px);
                unsafe {
                    gl.viewport(x, y, w, h);
                    gl.use_program(Some(scene.program));
                    let m = mvp(angle, aspect);
                    gl.uniform_matrix_4_f32_slice(scene.mvp_loc.as_ref(), false, &m);
                    gl.bind_vertex_array(Some(scene.vao));
                    gl.draw_arrays(glow::TRIANGLES, 0, 3);
                    gl.bind_vertex_array(None);
                    gl.use_program(None);
                }
            });
            ui.painter().add(egui::PaintCallback {
                rect,
                callback: Arc::new(callback),
            });
        });
    }

    fn on_exit(&mut self, gl: Option<&glow::Context>) {
        if let (Some(gl), Some(scene)) = (gl, self.scene.lock().take()) {
            unsafe {
                gl.delete_program(scene.program);
                gl.delete_vertex_array(scene.vao);
                gl.delete_buffer(scene.vbo);
            }
        }
    }
}

fn main() -> eframe::Result<()> {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default().with_inner_size([960.0, 600.0]),
        ..Default::default()
    };
    eframe::run_native(
        "Twin viewport spike",
        options,
        Box::new(|cc| Ok(Box::new(ViewportApp::new(cc)))),
    )
}
