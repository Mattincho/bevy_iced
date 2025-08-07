//! # Use Iced UI programs in your Bevy application
//!
//! ```no_run
//! use bevy::prelude::*;
//! use bevy_iced::iced::widget::text;
//! use bevy_iced::{IcedContext, IcedPlugin};
//!
//! #[derive(Event)]
//! pub enum UiMessage {}
//!
//! pub fn main() {
//!     App::new()
//!         .add_plugins(DefaultPlugins)
//!         .add_plugins(IcedPlugin::default())
//!         .add_event::<UiMessage>()
//!         .add_systems(Update, ui_system)
//!         .run();
//! }
//!
//! fn ui_system(time: Res<Time>, mut ctx: IcedContext<UiMessage>) {
//!     ctx.display(text(format!(
//!         "Hello Iced! Running for {:.2} seconds.",
//!         time.elapsed_seconds()
//!     )));
//! }
//! ```

#![deny(unsafe_code)]
#![deny(missing_docs)]

use std::any::{Any, TypeId};
use std::borrow::Cow;

use crate::render::{IcedNode, ViewportResource, extract_iced_data};

use bevy_app::{App, Plugin, Update};
use bevy_derive::{Deref, DerefMut};
use bevy_ecs::prelude::{EventWriter, Query, With};
use bevy_ecs::schedule::IntoSystemConfigs;
#[cfg(target_arch = "wasm32")]
use bevy_ecs::system::NonSend;
use bevy_ecs::system::{NonSendMut, Res, ResMut, Resource, SystemParam};
use bevy_input::touch::Touches;
use bevy_render::render_graph::RenderGraph;
use bevy_render::renderer::{RenderAdapter, RenderDevice, RenderQueue, render_system};
use bevy_render::{ExtractSchedule, Render, RenderApp, RenderSet};
use bevy_utils::HashMap;
use bevy_window::{PrimaryWindow, Window};
use cfg_if::cfg_if;
use iced_core::Theme;
use iced_core::mouse::Cursor;
use iced_runtime::user_interface::UserInterface;
use iced_wgpu::Engine;
use iced_widget::graphics::Viewport;

/// Basic re-exports for all Iced-related stuff.
///
/// This module attempts to emulate the `iced` package's API
/// as much as possible.
pub mod iced;

mod conversions;
mod render;
mod systems;
mod utils;

use render::TEXTURE_FMT;
use systems::IcedEventQueue;

/// The default renderer.
pub type Renderer = iced_renderer::Renderer;

/// The main feature of `bevy_iced`.
/// Add this to your [`App`] by calling `app.add_plugin(bevy_iced::IcedPlugin::default())`.
#[derive(Default)]
pub struct IcedPlugin {
    /// The settings that Iced should use.
    pub settings: iced::Settings,
    /// Font file contents
    pub fonts: Vec<&'static [u8]>,
}

impl Plugin for IcedPlugin {
    fn build(&self, app: &mut App) {
        app.add_systems(Update, (systems::process_input, render::update_viewport))
            .insert_resource(DidDraw::default())
            .insert_resource(IcedSettings::default())
            .insert_non_send_resource(IcedCache::default())
            .insert_resource(IcedEventQueue::default());
    }

    fn finish(&self, app: &mut App) {
        let default_viewport = Viewport::with_physical_size(iced_core::Size::new(1600, 900), 1.0);
        let default_viewport = ViewportResource(default_viewport);
        let iced_resource: IcedResource = IcedProps::new(app, self).into();

        app.insert_resource(default_viewport.clone());
        cfg_if! {
            if #[cfg(target_arch = "wasm32")] {
                app.insert_non_send_resource(iced_resource.clone());
            } else {
                app.insert_resource(iced_resource.clone());
            }
        }

        let render_app = app.sub_app_mut(RenderApp);
        render_app
            .insert_resource(default_viewport)
            .add_systems(ExtractSchedule, extract_iced_data)
            .add_systems(
                Render,
                render::recall_staging_belt
                    .after(render_system)
                    .in_set(RenderSet::Render),
            );
        cfg_if! {
            if #[cfg(target_arch = "wasm32")] {
                render_app.world_mut().insert_non_send_resource(iced_resource);
            } else {
                render_app.world_mut().insert_resource(iced_resource);
            }
        }
        setup_pipeline(&mut render_app.world_mut().get_resource_mut().unwrap());
    }
}

struct IcedProps {
    pub engine: Engine,
    renderer: Renderer,
    debug: iced_runtime::Debug,
}

impl IcedProps {
    fn new(app: &App, config: &IcedPlugin) -> Self {
        let render_world = &app.sub_app(RenderApp).world();
        let device = render_world
            .get_resource::<RenderDevice>()
            .unwrap()
            .wgpu_device();
        let queue = render_world.get_resource::<RenderQueue>().unwrap();
        let adapter = render_world.get_resource::<RenderAdapter>().unwrap();
        let engine = iced_wgpu::Engine::new(
            adapter,
            device,
            queue,
            TEXTURE_FMT,
            Some(iced_wgpu::graphics::Antialiasing::MSAAx4),
        );

        for &font in &config.fonts {
            iced_graphics::text::font_system()
                .write()
                .expect("write lock on global FontSystem")
                .load_font(Cow::from(font));
        }

        Self {
            renderer: iced_wgpu::Renderer::new(
                device,
                &engine,
                config.settings.default_font,
                config.settings.default_text_size,
            ),
            engine,
            debug: iced_runtime::Debug::new(),
        }
    }
}

#[cfg(target_arch = "wasm32")]
#[allow(private_interfaces)]
mod iced_resource {
    use super::*;

    use std::cell::{RefCell, RefMut};
    use std::rc::Rc;

    #[derive(Clone)]
    pub struct IcedResource(Rc<RefCell<IcedProps>>);

    impl IcedResource {
        pub fn lock(&self) -> RefMut<IcedProps> {
            self.0.borrow_mut()
        }
    }

    impl From<IcedProps> for IcedResource {
        fn from(value: IcedProps) -> Self {
            Self(Rc::new(RefCell::new(value)))
        }
    }
}

#[cfg(not(target_arch = "wasm32"))]
#[allow(private_interfaces)]
mod iced_resource {
    use super::*;

    use std::sync::{Arc, Mutex, MutexGuard};

    #[derive(Resource, Clone)]
    pub struct IcedResource(Arc<Mutex<IcedProps>>);

    impl IcedResource {
        pub fn lock(&self) -> MutexGuard<IcedProps> {
            self.0.lock().unwrap()
        }
    }

    impl From<IcedProps> for IcedResource {
        fn from(value: IcedProps) -> Self {
            Self(Arc::new(Mutex::new(value)))
        }
    }
}

use iced_resource::IcedResource;

fn setup_pipeline(graph: &mut RenderGraph) {
    graph.add_node(render::IcedPass, IcedNode);

    graph.add_node_edge(bevy_render::graph::CameraDriverLabel, render::IcedPass);
}

#[derive(Default)]
struct IcedCache {
    cache: HashMap<TypeId, Option<iced_runtime::user_interface::Cache>>,
}

impl IcedCache {
    fn get<M: Any>(&mut self) -> &mut Option<iced_runtime::user_interface::Cache> {
        let id = TypeId::of::<M>();
        if !self.cache.contains_key(&id) {
            self.cache.insert(id, Some(Default::default()));
        }
        self.cache.get_mut(&id).unwrap()
    }
}

/// Settings used to independently customize Iced rendering.
#[derive(Clone, Resource)]
pub struct IcedSettings {
    /// The scale factor to use for rendering Iced elements.
    /// Setting this to `None` defaults to using the `Window`s scale factor.
    pub scale_factor: Option<f64>,
    /// The theme to use for rendering Iced elements.
    pub theme: Theme,
    /// The style to use for rendering Iced elements.
    pub style: iced::Style,
}

impl IcedSettings {
    /// Set the `scale_factor` used to render Iced elements.
    pub fn set_scale_factor(&mut self, factor: impl Into<Option<f64>>) {
        self.scale_factor = factor.into();
    }
}

impl Default for IcedSettings {
    fn default() -> Self {
        Self {
            scale_factor: None,
            theme: Theme::Dark,
            style: iced::Style {
                text_color: iced_core::Color::WHITE,
            },
        }
    }
}

// An atomic flag for updating the draw state.
#[derive(Resource, Deref, DerefMut, Default)]
pub(crate) struct DidDraw(std::sync::atomic::AtomicBool);

/// The context for interacting with Iced. Add this as a parameter to your system.
/// ```ignore
/// fn ui_system(..., mut ctx: IcedContext<UiMessage>) {
///     let element = ...; // Build your element
///     ctx.display(element);
/// }
/// ```
///
/// `IcedContext<T>` requires an event system to be defined in the [`App`].
/// Do so by invoking `app.add_event::<T>()` when constructing your App.
#[derive(SystemParam)]
pub struct IcedContext<'w, 's, Message: bevy_ecs::event::Event> {
    viewport: Res<'w, ViewportResource>,
    #[cfg(target_arch = "wasm32")]
    props: NonSend<'w, IcedResource>,
    #[cfg(not(target_arch = "wasm32"))]
    props: Res<'w, IcedResource>,
    settings: Res<'w, IcedSettings>,
    windows: Query<'w, 's, &'static Window, With<PrimaryWindow>>,
    events: ResMut<'w, IcedEventQueue>,
    cache_map: NonSendMut<'w, IcedCache>,
    messages: EventWriter<'w, Message>,
    did_draw: ResMut<'w, DidDraw>,
    touches: Res<'w, Touches>,
}

impl<M: bevy_ecs::event::Event> IcedContext<'_, '_, M> {
    /// Display an [`Element`] to the screen.
    pub fn display<'a>(
        &'a mut self,
        element: impl Into<iced_core::Element<'a, M, Theme, Renderer>>,
    ) {
        let &mut IcedProps {
            ref mut renderer, ..
        } = &mut *self.props.lock();
        let bounds = self.viewport.logical_size();

        let element = element.into();

        if self.windows.get_single().is_err() {
            return;
        }

        let cursor = {
            let window = self.windows.single();
            match window.cursor_position() {
                Some(position) => {
                    Cursor::Available(utils::process_cursor_position(position, bounds, window))
                }
                None => utils::process_touch_input(self)
                    .map(Cursor::Available)
                    .unwrap_or(Cursor::Unavailable),
            }
        };

        self.events.push(iced_core::Event::Window(
            iced_core::window::Event::RedrawRequested(bevy_utils::Instant::now()),
        ));

        let mut messages = Vec::<M>::new();
        let cache_entry = self.cache_map.get::<M>();
        let cache = cache_entry.take().unwrap();
        let mut ui = UserInterface::build(element, bounds, cache, renderer);
        let (_, _event_statuses) = ui.update(
            self.events.as_slice(),
            cursor,
            renderer,
            &mut iced_core::clipboard::Null,
            &mut messages,
        );

        messages.into_iter().for_each(|msg| {
            self.messages.send(msg);
        });

        ui.draw(renderer, &self.settings.theme, &self.settings.style, cursor);

        self.events.clear();
        *cache_entry = Some(ui.into_cache());
        self.did_draw
            .store(true, std::sync::atomic::Ordering::Relaxed);
    }
}
