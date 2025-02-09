use flume::{Receiver, Sender};
use shared::primitive::Size;

use render::{InputEvent, OutputEvent, RenderEngine};
use url::Url;

pub struct RenderClient {
    event_sender: Sender<InputEvent>,
    event_receiver: Receiver<OutputEvent>,
    ready_receiver: Receiver<()>,
}

impl RenderClient {
    pub fn new() -> Self {
        let (render_input_tx, render_input_rx) = flume::unbounded();
        let (render_output_tx, render_output_rx) = flume::unbounded();

        let (ready_tx, ready_rx) = flume::bounded(1);

        // spawn a new thread to run render engine
        let _ = std::thread::spawn(move || {
            let rt = tokio::runtime::Runtime::new().unwrap();
            rt.block_on(async {
                let render_engine = RenderEngine::new(Size::new(1., 1.)).await;

                ready_tx.send(()).unwrap();

                // run render engine (this is an infinite loop)
                if let Err(e) = render_engine.run(render_input_rx, render_output_tx).await {
                    log::error!("Render Engine exited with error: {}", e.to_string());
                }
            });
        });

        Self {
            event_sender: render_input_tx,
            event_receiver: render_output_rx,
            ready_receiver: ready_rx,
        }
    }

    pub fn wait_till_ready(&self) {
        self.ready_receiver
            .recv()
            .expect("Error while waiting for render client to be ready")
    }

    pub fn events(&self) -> Receiver<OutputEvent> {
        self.event_receiver.clone()
    }

    pub fn load_html(&self, html: String, base_url: Url) {
        self.event_sender
            .send(InputEvent::LoadHTML { html, base_url })
            .expect("Unable to load HTML");
    }

    pub fn resize(&self, size: Size) {
        self.event_sender
            .send(InputEvent::ViewportResize(size))
            .expect("Unable to load HTML");
    }
}
