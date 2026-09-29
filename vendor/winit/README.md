# Vendored winit 0.30.13

Patched copy of [winit 0.30.13](https://crates.io/crates/winit/0.30.13) with Wayland
trackpad pinch/pan/rotation support backported from
[winit#4338](https://github.com/rust-windowing/winit/pull/4338).

Upstream 0.30 only emits `PinchGesture` on macOS/iOS. On Wayland the compositor
(Hyprland, Mutter, …) advertises `zwp_pointer_gestures_v1`, but winit never bound
it — so pinches were dropped and Marker saw nothing. This tree binds the global
and forwards pinch/pan/rotation as `WindowEvent`s that egui-winit already maps.

Remove this patch once Marker moves to a winit release that includes Wayland
gestures (0.31+).
