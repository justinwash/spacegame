use crate::assets::sprite::SpriteAsset;
use crate::assets::AssetManager;
use crate::core::camera::ProjectionMatrix;
use crate::render::particle::ParticleSystem;
use crate::render::sprite::SpriteRenderer;
use crate::render::ui::{Gui, UiRenderer};
use crate::resources::Resources;
use luminance::context::GraphicsContext;
use luminance::framebuffer::Framebuffer;
use luminance::pipeline::{PipelineError, PipelineState, Render};
use luminance::texture::Dim2;
use luminance_gl::GL33;
use std::time::Duration;

pub mod background;
pub mod particle;
pub mod sprite;
pub mod ui;

pub struct Renderer<S>
where
    S: GraphicsContext<Backend = GL33>,
{
    /// Render sprites on screen.
    sprite_renderer: SpriteRenderer<S>,

    /// particles :)
    particle_renderer: ParticleSystem<S>,

    ui_renderer: UiRenderer<S>,
}

impl<S> Renderer<S>
where
    S: GraphicsContext<Backend = GL33> + 'static,
{
    pub fn new(surface: &mut S) -> Renderer<S> {
        let sprite_renderer = sprite::SpriteRenderer::new(surface);

        let particle_renderer = ParticleSystem::new(surface);
        let ui_renderer = UiRenderer::new(surface);
        Self {
            sprite_renderer,
            particle_renderer,
            ui_renderer,
        }
    }

    pub fn prepare_ui(&mut self, surface: &mut S, gui: Gui, resources: &Resources) {
        self.ui_renderer.prepare(surface, gui, resources);
    }

    pub fn render(
        &mut self,
        surface: &mut S,
        back_buffer: &mut Framebuffer<S::Backend, Dim2, (), ()>,
        world: &hecs::World,
        resources: &Resources,
    ) -> Render<PipelineError> {
        let projection_matrix = resources.fetch::<ProjectionMatrix>().unwrap().0;
        let view = crate::core::camera::get_view_matrix(world).unwrap();

        let mut textures = resources
            .fetch_mut::<AssetManager<S, SpriteAsset<S>>>()
            .unwrap();
        surface
            .new_pipeline_gate()
            .pipeline(
                back_buffer,
                &PipelineState::default().set_clear_color([0.0, 0.0, 0.0, 1.0]),
                |pipeline, mut shd_gate| {
                    self.sprite_renderer.render(
                        &pipeline,
                        &mut shd_gate,
                        &projection_matrix,
                        &view,
                        &world,
                        &mut *textures,
                    )?;

                    self.particle_renderer.render(
                        &mut shd_gate,
                        &projection_matrix,
                        &view,
                        world,
                    )?;

                    self.ui_renderer.render(&pipeline, &mut shd_gate)
                },
            )
            .assume()
    }

    pub fn update(
        &mut self,
        _surface: &mut S,
        world: &hecs::World,
        dt: Duration,
        resources: &Resources,
    ) {
        // update particle systems.
        self.particle_renderer.update(world, dt, resources);
    }
}
