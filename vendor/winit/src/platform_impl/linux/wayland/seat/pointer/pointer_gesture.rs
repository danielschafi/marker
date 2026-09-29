//! Wayland pointer gestures (`zwp_pointer_gestures_v1`).
//!
//! Backported from winit PR #4338 so trackpad pinch reaches egui on Linux.

use std::ops::Deref;
use std::sync::Mutex;

use sctk::compositor::SurfaceData;
use sctk::globals::GlobalData;
use sctk::reexports::client::globals::{BindError, GlobalList};
use sctk::reexports::client::{delegate_dispatch, Connection, Dispatch, Proxy, QueueHandle};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gesture_pinch_v1::{
    Event, ZwpPointerGesturePinchV1,
};
use sctk::reexports::protocols::wp::pointer_gestures::zv1::client::zwp_pointer_gestures_v1::ZwpPointerGesturesV1;

use crate::dpi::{LogicalPosition, PhysicalPosition};
use crate::event::{TouchPhase, WindowEvent};
use crate::platform_impl::wayland::state::WinitState;
use crate::platform_impl::wayland::{self, DeviceId};

/// Wrapper around the pointer gesture global.
#[derive(Debug)]
pub struct PointerGesturesState {
    pointer_gestures: ZwpPointerGesturesV1,
}

impl PointerGesturesState {
    /// Bind `zwp_pointer_gestures_v1` when the compositor advertises it.
    pub fn new(
        globals: &GlobalList,
        queue_handle: &QueueHandle<WinitState>,
    ) -> Result<Self, BindError> {
        // Hyprland/Mutter expose v3; v1 is enough for pinch.
        let pointer_gestures = globals.bind(queue_handle, 1..=3, GlobalData)?;
        Ok(Self { pointer_gestures })
    }
}

#[derive(Debug, Default)]
pub struct PointerGestureData {
    inner: Mutex<PointerGestureDataInner>,
}

#[derive(Debug)]
struct PointerGestureDataInner {
    window_id: Option<wayland::WindowId>,
    previous_pinch: f64,
}

impl Default for PointerGestureDataInner {
    fn default() -> Self {
        Self {
            window_id: None,
            previous_pinch: 1.0,
        }
    }
}

impl Deref for PointerGesturesState {
    type Target = ZwpPointerGesturesV1;

    fn deref(&self) -> &Self::Target {
        &self.pointer_gestures
    }
}

impl Dispatch<ZwpPointerGesturesV1, GlobalData, WinitState> for PointerGesturesState {
    fn event(
        _state: &mut WinitState,
        _proxy: &ZwpPointerGesturesV1,
        _event: <ZwpPointerGesturesV1 as Proxy>::Event,
        _data: &GlobalData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        unreachable!("zwp_pointer_gestures_v1 has no events")
    }
}

impl Dispatch<ZwpPointerGesturePinchV1, PointerGestureData, WinitState> for PointerGesturesState {
    fn event(
        state: &mut WinitState,
        _proxy: &ZwpPointerGesturePinchV1,
        event: <ZwpPointerGesturePinchV1 as Proxy>::Event,
        data: &PointerGestureData,
        _conn: &Connection,
        _qhandle: &QueueHandle<WinitState>,
    ) {
        let mut pointer_gesture_data = data.inner.lock().unwrap();
        let device_id =
            crate::event::DeviceId(crate::platform_impl::DeviceId::Wayland(DeviceId));

        let (window_id, phase, pan_delta, pinch_delta, rotation_delta) = match event {
            Event::Begin {
                surface, fingers, ..
            } => {
                // Two-finger trackpad pinch only.
                if fingers != 2 {
                    return;
                }

                // Ignore subsurfaces (e.g. CSD).
                if surface
                    .data::<SurfaceData>()
                    .is_some_and(|data| data.parent_surface().is_some())
                {
                    return;
                }

                let window_id = wayland::make_wid(&surface);
                pointer_gesture_data.window_id = Some(window_id);
                pointer_gesture_data.previous_pinch = 1.0;

                (
                    window_id,
                    TouchPhase::Started,
                    PhysicalPosition::new(0., 0.),
                    0.,
                    0.,
                )
            },
            Event::Update {
                dx,
                dy,
                scale: pinch,
                rotation,
                ..
            } => {
                let window_id = match pointer_gesture_data.window_id {
                    Some(window_id) => window_id,
                    None => return,
                };

                let scale_factor = match state.windows.get_mut().get(&window_id) {
                    Some(window) => window.lock().unwrap().scale_factor(),
                    None => return,
                };

                let pan_delta =
                    LogicalPosition::new(dx as f32, dy as f32).to_physical(scale_factor);

                let pinch_delta = pinch - pointer_gesture_data.previous_pinch;
                pointer_gesture_data.previous_pinch = pinch;

                // Wayland rotation is degrees CW; winit uses degrees CCW.
                let rotation_delta = -rotation as f32;
                (
                    window_id,
                    TouchPhase::Moved,
                    pan_delta,
                    pinch_delta,
                    rotation_delta,
                )
            },
            Event::End { cancelled, .. } => {
                let window_id = match pointer_gesture_data.window_id {
                    Some(window_id) => window_id,
                    None => return,
                };

                *pointer_gesture_data = PointerGestureDataInner::default();

                let phase = if cancelled == 0 {
                    TouchPhase::Ended
                } else {
                    TouchPhase::Cancelled
                };
                (
                    window_id,
                    phase,
                    PhysicalPosition::new(0., 0.),
                    0.,
                    0.,
                )
            },
            _ => return,
        };

        state.events_sink.push_window_event(
            WindowEvent::PanGesture {
                device_id,
                delta: pan_delta,
                phase,
            },
            window_id,
        );
        state.events_sink.push_window_event(
            WindowEvent::PinchGesture {
                device_id,
                delta: pinch_delta,
                phase,
            },
            window_id,
        );
        state.events_sink.push_window_event(
            WindowEvent::RotationGesture {
                device_id,
                delta: rotation_delta,
                phase,
            },
            window_id,
        );
        state.dispatched_events = true;
    }
}

delegate_dispatch!(WinitState: [ZwpPointerGesturesV1: GlobalData] => PointerGesturesState);
delegate_dispatch!(WinitState: [ZwpPointerGesturePinchV1: PointerGestureData] => PointerGesturesState);
