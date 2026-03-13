use scap::{
    capturer::{Point, Area, Size, Capturer, Options},
    frame::Frame,
};

use pipewire::{
    main_loop::MainLoopBox,
    context::ContextBox
};

mod targets;
mod utils;

// Helper Methods
pub use targets::{get_all_targets, get_main_display};
pub use targets::{Display, Target};
pub use utils::has_permission;
pub use utils::is_supported;
pub use utils::request_permission;

//entry
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mainloop = MainLoopBox::new(scap_entry)?;
    let context = ContextBox::new(&mainloop.loop_(), None)?;
    let core = context.connect(None)?;
    let registry = core.get_registry()?;

    Ok(())
}
//scap
fn scap_entry() {
    // Check if the platform is supported
    if !scap::is_supported() {
        println!("❌ Platform not supported");
        return;
    }

    // Check if we have permission to capture screen
    // If we don't, request it.
    if !scap::has_permission() {
        println!("❌ Permission not granted. Requesting permission...");
        if !scap::request_permission() {
            println!("❌ Permission denied");
            return;
        }
    }

    // Get recording targets
    let targets = scap::get_all_targets();
    println!("Targets: {:?}", targets);

    // All your displays and windows are targets
    // You can filter this and capture the one you need.

    // Create Options
    let options = Options {
        fps: 60,
        target: None, // None captures the primary display
        show_cursor: true,
        show_highlight: true,
        excluded_targets: None,
        output_type: scap::frame::FrameType::BGRAFrame,
        output_resolution: scap::capturer::Resolution::_720p,
        crop_area: Some(Area {
            origin: Point { x: 0.0, y: 0.0 },
            size: Size {
                width: 2000.0,
                height: 1000.0,
            },
        }),
        ..Default::default()
    };

    // Create Capturer
    let mut capturer = Capturer::build(options).unwrap();

    // Start Capture
    capturer.start_capture();

    let mut input = String::new();
    std::io::stdin().read_line(&mut input).unwrap();

    // Stop Capture
    capturer.stop_capture();
}


