use std::cell::RefCell;
use std::ops::Deref;
use std::rc::Rc;

use libpulse_binding as pulse;
use libpulse_binding::callbacks::ListResult;
use libpulse_binding::context::introspect::{SinkInfo, SourceInfo};
use libpulse_binding::context::{Context, FlagSet, State};
use libpulse_binding::mainloop::standard::{IterateResult, Mainloop};
use libpulse_binding::proplist::Proplist;
use log::warn;
use std::time::{Duration, Instant};

pub(crate) struct PulseAudioConfiguration;
pub struct PulseRuntime {
    main_loop: Rc<RefCell<Mainloop>>,
    context: Rc<RefCell<Context>>,
}

// How long to wait for the PulseAudio (or PipeWire) server before giving up on this attempt
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);

impl PulseRuntime {
    /// Connects to the server, or None when it's unavailable or refuses (no panic, no endless
    /// wait: this runs from the sampler's threads).
    fn connect() -> Option<Self> {
        // Connect to the PulseAudio Server..
        let app_name: &str = env!("CARGO_PKG_NAME");

        let mut proplist = Proplist::new()?;
        proplist
            .set_str(pulse::proplist::properties::APPLICATION_NAME, app_name)
            .ok()?;

        let main_loop = Rc::new(RefCell::new(Mainloop::new()?));
        let context = Rc::new(RefCell::new(Context::new_with_proplist(
            main_loop.borrow().deref(),
            app_name,
            &proplist,
        )?));

        if let Err(e) = context.borrow_mut().connect(None, FlagSet::NOFLAGS, None) {
            warn!("Unable to connect to PulseAudio: {}", e);
            return None;
        }

        let deadline = Instant::now() + CONNECT_TIMEOUT;
        loop {
            match main_loop.borrow_mut().iterate(false) {
                IterateResult::Success(_) => {}
                IterateResult::Quit(_) | IterateResult::Err(_) => {
                    warn!("Failed to Connect to Pulse Audio!");
                    return None;
                }
            }

            match context.borrow().get_state() {
                State::Ready => break,
                State::Failed | State::Terminated => {
                    warn!("PulseAudio refused the connection");
                    return None;
                }
                State::Unconnected
                | State::Connecting
                | State::Authorizing
                | State::SettingName => {}
            }
            if Instant::now() > deadline {
                warn!("PulseAudio didn't answer in time");
                return None;
            }
            std::thread::sleep(Duration::from_millis(5));
        }

        // At this point, we're connected and ready to go :)
        Some(PulseRuntime { main_loop, context })
    }

    fn disconnect(&self) {
        self.context.borrow_mut().disconnect();
    }

    /// Runs the main loop until an operation is done (or the server stops answering).
    fn wait_for<T: ?Sized>(&self, op: &pulse::operation::Operation<T>) {
        let deadline = Instant::now() + CONNECT_TIMEOUT;
        while op.get_state() == pulse::operation::State::Running && Instant::now() < deadline {
            if let IterateResult::Quit(_) | IterateResult::Err(_) =
                self.main_loop.borrow_mut().iterate(false)
            {
                break;
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    }
}

impl Drop for PulseRuntime {
    fn drop(&mut self) {
        // We need to disconnect our context before we go out of scope, otherwise we'll
        // segfault when libpulse tries to drop.
        self.context.borrow_mut().disconnect();
    }
}

impl PulseAudioConfiguration {
    pub(crate) fn get_outputs() -> Vec<String> {
        let found: Vec<String> = vec![];
        let wrapped = Rc::new(RefCell::new(found));
        let insider = wrapped.clone();

        let Some(pulse) = PulseRuntime::connect() else {
            return vec![];
        };

        let op = {
            pulse.context.borrow_mut().introspect().get_sink_info_list(
                move |sink_list: ListResult<&SinkInfo>| {
                    if let ListResult::Item(item) = sink_list
                        && let Some(name) = &item.name
                    {
                        insider.borrow_mut().push(name.to_string());
                    }
                },
            )
        };

        // Block here until the above closure has completed..
        pulse.wait_for(&op);

        pulse.disconnect();

        wrapped.deref().borrow().clone()
    }

    pub(crate) fn get_inputs() -> Vec<String> {
        let Some(pulse) = PulseRuntime::connect() else {
            return vec![];
        };

        // Basically identical to the above, except getting the Sources..
        let found: Vec<String> = vec![];
        let wrapped = Rc::new(RefCell::new(found));
        let insider = wrapped.clone();

        let op = {
            pulse
                .context
                .borrow_mut()
                .introspect()
                .get_source_info_list(
                    move |source_list: ListResult<&SourceInfo>| match source_list {
                        ListResult::Item(item) => {
                            if let Some(name) = &item.name {
                                insider.borrow_mut().push(name.to_string());
                            }
                        }
                        ListResult::End => {}
                        ListResult::Error => {}
                    },
                )
        };

        // Block here until the above closure has completed..
        pulse.wait_for(&op);

        pulse.disconnect();

        wrapped.deref().borrow().clone()
    }
}
