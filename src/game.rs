#[cfg(feature = "hot-reload")]
use crate::assets::HotReloader;
use crate::config::AudioConfig;
use crate::core::audio::AudioSystem;
use crate::core::camera::{Camera, ProjectionMatrix};
use crate::core::input::{Input, InputAction};
use crate::core::random::{RandomGenerator, Seed};
use crate::core::scene::{Scene, SceneStack};
use crate::core::transform::update_transforms;
use crate::core::window::WindowDim;
use crate::event::GameEvent;
use crate::gameplay::collision::CollisionWorld;
use crate::gameplay::delete::GarbageCollector;
use crate::render::path::debug::DebugQueue;
use crate::render::ui::gui::GuiContext;
use crate::render::Renderer;
use crate::resources::Resources;
use crate::{HEIGHT, WIDTH};
use glfw::{Context, Key, MouseButton, WindowEvent};
use log::info;
use luminance_glfw::GlfwSurface;
use shrev::{EventChannel, ReaderId};
use std::any::Any;
use std::collections::HashMap;
use std::marker::PhantomData;
use std::thread;
use std::time::{Duration, Instant};

/// GameBuilder is used to create a new game. Game struct has a lot of members that do not need to be
/// exposed so gamebuilder provides a simpler way to get started.
pub struct GameBuilder<'a, A>
where
    A: InputAction,
{
    surface: &'a mut GlfwSurface,
    scene: Option<Box<dyn Scene<WindowEvent>>>,
    resources: Resources,
    phantom: PhantomData<A>,
    seed: Option<Seed>,
    input_config: Option<(HashMap<Key, A>, HashMap<MouseButton, A>)>,
    gui_context: GuiContext,
    audio_config: AudioConfig,
}

impl<'a, A> GameBuilder<'a, A>
where
    A: InputAction + 'static,
{
    pub fn new(surface: &'a mut GlfwSurface) -> Self {
        // resources will need at least an event channel and an input
        let mut resources = Resources::default();
        let chan: EventChannel<GameEvent> = EventChannel::new();
        resources.insert(chan);

        // and some asset manager;
        crate::assets::create_asset_managers(surface, &mut resources);

        // the proj matrix.
        resources.insert(ProjectionMatrix::new(WIDTH as f32, HEIGHT as f32));
        resources.insert(WindowDim::new(WIDTH, HEIGHT));
        resources.insert(CollisionWorld::default());
        resources.insert(DebugQueue::default());

        Self {
            gui_context: GuiContext::new(WindowDim::new(WIDTH, HEIGHT)),
            surface,
            scene: None,
            resources,
            input_config: None,
            phantom: PhantomData::default(),
            seed: None,
            audio_config: AudioConfig::default(),
        }
    }

    /// Set up the first scene.
    pub fn for_scene(mut self, scene: Box<dyn Scene<WindowEvent>>) -> Self {
        self.scene = Some(scene);
        self
    }

    pub fn with_input_config(
        mut self,
        key_map: HashMap<Key, A>,
        btn_map: HashMap<MouseButton, A>,
    ) -> Self {
        self.input_config = Some((key_map, btn_map));
        self
    }

    /// Specific config for audio
    pub fn with_audio_config(mut self, audio_config: AudioConfig) -> Self {
        self.audio_config = audio_config;
        self
    }

    /// Add custom resources.
    pub fn with_resource<T: Any>(mut self, r: T) -> Self {
        self.resources.insert(r);
        self
    }

    pub fn with_seed(mut self, seed: Seed) -> Self {
        self.seed = Some(seed);
        self
    }

    pub fn build(mut self) -> Game<'a, A> {
        let renderer = Renderer::new(self.surface, &self.gui_context);
        // Need some input :D
        let input: Input<A> = {
            let (key_mapping, btn_mapping) = self
                .input_config
                .unwrap_or((A::get_default_key_mapping(), A::get_default_mouse_mapping()));
            Input::new(key_mapping, btn_mapping)
        };
        self.resources.insert(input);
        let mut world = hecs::World::new();

        // if a seed is provided, let's add it to the resources.
        if let Some(seed) = self.seed {
            self.resources.insert(RandomGenerator::new(seed));
        } else {
            self.resources.insert(RandomGenerator::from_entropy());
        }

        let scene_stack = {
            let mut scenes = SceneStack::default();
            if let Some(scene) = self.scene {
                scenes.push(scene, &mut world, &mut self.resources);
            }
            scenes
        };

        let rdr_id = {
            let mut chan = self
                .resources
                .fetch_mut::<EventChannel<GameEvent>>()
                .unwrap();
            chan.register_reader()
        };

        let garbage_collector = GarbageCollector::new(&mut self.resources);

        // we need a camera :)
        world.spawn((Camera::new(),));

        info!("Finished building game");

        // audio system.
        let audio_system = AudioSystem::new(&self.resources, self.audio_config)
            .expect("Cannot create audio system");

        Game {
            surface: self.surface,
            renderer,
            scene_stack,
            world,
            audio_system,
            resources: self.resources,
            rdr_id,
            garbage_collector,
            phantom: self.phantom,
            gui_context: self.gui_context,
            #[cfg(feature = "hot-reload")]
            hot_reloader: HotReloader::new(),
        }
    }
}

/// Struct that holds the game state and systems.
///
/// # Lifetime requirement:
/// The opengl context is held in GlfwSurface. This is a mutable reference here as we do not want the
/// context to be dropped at the same time as the systems. If it is dropped before, then releasing GPU
/// resources will throw a segfault.
///
/// # Generic parameters:
/// - A: Action that is derived from the inputs. (e.g. Move Left)
///
pub struct Game<'a, A> {
    /// for drawing stuff
    surface: &'a mut GlfwSurface,
    renderer: Renderer<GlfwSurface>,

    /// All the scenes. Current scene will be used in the main loop.
    scene_stack: SceneStack<WindowEvent>,

    /// Play music and sound effects
    audio_system: AudioSystem,

    /// Resources (assets, inputs...)
    resources: Resources,

    /// Current entities.
    world: hecs::World,

    /// Read events from the systems
    rdr_id: ReaderId<GameEvent>,

    /// Clean up the dead entities.
    garbage_collector: GarbageCollector,

    gui_context: GuiContext,

    phantom: PhantomData<A>,

    #[cfg(feature = "hot-reload")]
    hot_reloader: HotReloader<GlfwSurface>,
}

impl<'a, A> Game<'a, A>
where
    A: InputAction + 'static,
{
    /// Run the game. This is the main loop.
    pub fn run(&mut self) {
        let mut current_time = Instant::now();
        let dt = Duration::from_millis(16);
        let mut back_buffer = self.surface.back_buffer().unwrap();

        'app: loop {
            // 1. Poll the events and update the Input resource
            // ------------------------------------------------
            let mut resize = false;
            self.surface.window.glfw.poll_events();
            {
                let mut input = self.resources.fetch_mut::<Input<A>>().unwrap();
                input.prepare();
                self.gui_context.reset_inputs();
                for (_, event) in self.surface.events_rx.try_iter() {
                    match event {
                        WindowEvent::Close => break 'app,
                        WindowEvent::FramebufferSize(_, _) => resize = true,
                        ev => {
                            self.gui_context.process_event(ev.clone());
                            if let Some(scene) = self.scene_stack.current_mut() {
                                scene.process_input(&mut self.world, ev.clone(), &self.resources);
                            }
                            input.process_event(ev)
                        }
                    }
                }
            }

            // 2. Update the scene.
            // ------------------------------------------------
            let scene_result = if let Some(scene) = self.scene_stack.current_mut() {
                let scene_res = scene.update(dt, &mut self.world, &self.resources);

                {
                    let chan = self.resources.fetch::<EventChannel<GameEvent>>().unwrap();
                    for ev in chan.read(&mut self.rdr_id) {
                        scene.process_event(&mut self.world, ev.clone(), &self.resources);
                    }
                }

                let maybe_gui =
                    scene.prepare_gui(dt, &mut self.world, &self.resources, &mut self.gui_context);

                self.renderer.prepare_ui(
                    self.surface,
                    maybe_gui,
                    &self.resources,
                    &mut *self.gui_context.fonts.borrow_mut(),
                );

                Some(scene_res)
            } else {
                None
            };

            // Update children transforms:
            // -----------------------------
            update_transforms(&mut self.world);

            // 3. Clean up dead entities.
            // ------------------------------------------------
            self.garbage_collector
                .collect(&mut self.world, &self.resources);

            // 4. Render to screen
            // ------------------------------------------------
            log::debug!("RENDER");
            self.renderer
                .update(self.surface, &self.world, dt, &self.resources);
            if resize {
                back_buffer = self.surface.back_buffer().unwrap();
                let new_size = back_buffer.size();
                let mut proj = self.resources.fetch_mut::<ProjectionMatrix>().unwrap();
                proj.resize(new_size[0] as f32, new_size[1] as f32);

                let mut dim = self.resources.fetch_mut::<WindowDim>().unwrap();
                dim.resize(new_size[0], new_size[1]);
                self.gui_context.window_dim = *dim;
            }

            let render =
                self.renderer
                    .render(self.surface, &mut back_buffer, &self.world, &self.resources);
            if render.is_ok() {
                self.surface.window.swap_buffers();
            } else {
                break 'app;
            }

            // Play music :)
            self.audio_system.process(&self.resources);

            // Update collision world for collision queries.
            {
                let mut collisions = self.resources.fetch_mut::<CollisionWorld>().unwrap();
                collisions.synchronize(&self.world);
            }

            // Either clean up or load new resources.
            crate::assets::update_asset_managers(self.surface, &self.resources);
            #[cfg(feature = "hot-reload")]
            self.hot_reloader.update(&self.resources);

            // Now, if need to switch scenes, do it.
            if let Some(res) = scene_result {
                self.scene_stack
                    .apply_result(res, &mut self.world, &mut self.resources);
            }

            let now = Instant::now();
            let frame_duration = now - current_time;
            if frame_duration < dt {
                thread::sleep(dt - frame_duration);
            }
            current_time = now;
        }

        info!("Bye bye.");
    }
}
