//! Hold-to-zoom camera control.

use std::sync::Mutex;

use weft_sdk::*;

static ZOOM_KEY: Key = Key::named("zoom");
static ZOOM_IN: Key = Key::named("zoom_in");
static ZOOM_OUT: Key = Key::named("zoom_out");

const DEFAULT_ZOOM: f32 = 5.0;
const MAX_ZOOM: f32 = 50.0;
const ZOOM_PER_NOTCH: f32 = 0.1;
const ZOOM_RESPONSE: f32 = 14.0;

struct Zoom {
    magnification: f32,
    fov: f32,
}

pub fn register(r: &mut Registry) {
    use abi::comp::key::DEBUG;
    r.key(&ZOOM_KEY, "Z").tag(DEBUG);
    r.key(&ZOOM_IN, "wheel_up").while_held(&ZOOM_KEY).tag(DEBUG);
    r.key(&ZOOM_OUT, "wheel_down")
        .while_held(&ZOOM_KEY)
        .tag(DEBUG);

    let zoom = Mutex::new(Zoom { magnification: DEFAULT_ZOOM, fov: 1.0 });
    r.camera_frame(move |a| {
        let keys = a.keys();
        let held = keys.held(&ZOOM_KEY);
        let delta = keys.state(&ZOOM_IN).amount - keys.state(&ZOOM_OUT).amount;
        let mut zoom = zoom.lock().unwrap();
        if held {
            zoom.magnification = (zoom.magnification * (delta * ZOOM_PER_NOTCH).exp()).clamp(1.0, MAX_ZOOM);
        } else {
            zoom.magnification = DEFAULT_ZOOM;
        }
        let target = if held { 1.0 / zoom.magnification } else { 1.0 };
        let blend = 1.0 - (-ZOOM_RESPONSE * a.dt.max(0.0)).exp();
        zoom.fov += (target - zoom.fov) * blend;
        a.out.fov *= zoom.fov;
        if !held {
            a.lod_fov_scale /= zoom.fov;
        }
    });
}
