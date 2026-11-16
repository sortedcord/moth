//! moth: a native video client for the Linux desktop.
//!
//! - [`backend`]: video sources (YouTube), the media model, stream selection and the player. No UI.
//! - [`ui`]: the gpui frontend.
//! - [`desktop`]: the instance socket, `moth status`, MPRIS and the browser's native host.
//! - [`settings`]: user preferences, shared by all of the above.

mod backend;
mod desktop;
mod dev;
mod paths;
mod settings;
mod sync;
mod ui;

use desktop::{mpris, native_host, socket, status};

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if native_host::is_invocation(&args) {
        std::process::exit(native_host::run());
    }
    // `moth status [--watch]`: report what the running instance is doing, as JSON lines.
    if args.first().is_some_and(|arg| arg == "status") {
        let watch = args.iter().skip(1).any(|arg| arg == "--watch" || arg == "-w");
        std::process::exit(socket::run_status_cli(watch));
    }
    // `moth <url>`: hand it to a running instance if there is one.
    let link = args.into_iter().find(|arg| !arg.starts_with('-'));
    if let Some(link) = &link
        && socket::send_to_running(link).is_ok()
    {
        return;
    }

    let dev = dev::DevOptions::from_env();
    if let Err(err) = backend::player::init() {
        eprintln!("moth: {err:#}");
        std::process::exit(1);
    }
    let backend = match backend::Backend::new(&dev) {
        Ok(backend) => backend,
        Err(err) => {
            eprintln!("moth: failed to start: {err:#}");
            std::process::exit(1);
        }
    };

    // Commands from the socket and MPRIS, handled on the UI thread as they arrive.
    let (commands, command_queue) = async_channel::unbounded();
    let hub = status::Hub::new();
    if let Err(err) = socket::listen(commands.clone(), hub.clone()) {
        eprintln!("moth: links from other launches and `moth status` won't reach this window: {err:#}");
    }
    let mpris = mpris::Mpris::start(hub.clone(), commands)
        .inspect_err(|err| eprintln!("moth: MPRIS unavailable (media keys and bars won't see moth): {err}"))
        .ok();
    hub.start_worker(move |previous, now| {
        if let Some(mpris) = &mpris {
            mpris.changed(previous, now);
        }
    });

    ui::run(backend, hub, command_queue, link, dev);
}
