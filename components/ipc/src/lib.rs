mod client;
use client::Client;
use flume::{Receiver, RecvError, Selector, Sender};
use std::ops::Deref;
use std::sync::{Arc, Mutex};
use std::{
    net::{SocketAddr, TcpListener, TcpStream},
    thread,
};

pub use client::{IpcTransportError, Message};

pub enum IpcMainReceiveError {
    NoConnections,
    Other(RecvError),
}

pub struct IpcMain<M: Message> {
    clients: Arc<Mutex<Vec<Client<M>>>>,
}

pub struct IpcRenderer<M: Message> {
    client: Client<M>,
}

pub struct IpcConnection<M> {
    pub sender: Sender<M>,
    pub receiver: Receiver<M>,
}

impl<M: Message> IpcMain<M> {
    pub fn new() -> Self {
        Self {
            clients: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn run(&mut self, port: u16) {
        let clients = self.clients.clone();

        thread::spawn(move || {
            let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], port)))
                .expect("Unable to bind port");

            for stream in listener.incoming() {
                let stream_read = stream.expect("Unable to obtain read stream");
                let stream_write = stream_read
                    .try_clone()
                    .expect("Unable to obtain write stream");
                let client = Client::<M>::new(|| stream_read, || stream_write);

                clients.lock().unwrap().push(client);
            }
        });
    }

    pub fn receive(&self) -> Result<(Sender<M>, M), IpcMainReceiveError> {
        let clients = &*self.clients.lock().unwrap();

        if clients.len() == 0 {
            return Err(IpcMainReceiveError::NoConnections);
        }

        let mut selector = Selector::new();

        for (index, renderer) in clients.iter().enumerate() {
            let index = index.clone();
            selector = selector.recv(renderer.receiver(), move |msg| (index, msg));
        }

        let (index, msg) = selector.wait();
        let msg = msg.map_err(|e| IpcMainReceiveError::Other(e))?;

        Ok((clients[index].sender().clone(), msg))
    }

    pub fn get_connection(&self, index: usize) -> IpcConnection<M> {
        let clients = self.clients.lock().unwrap();
        let client = &clients[index];

        IpcConnection {
            sender: client.sender.clone(),
            receiver: client.receiver.clone(),
        }
    }
}

impl<M: Message> IpcRenderer<M> {
    pub fn new(port: u16) -> Self {
        let (stream_read, stream_write) = loop {
            if let Ok(stream_read) = TcpStream::connect(SocketAddr::from(([127, 0, 0, 1], port))) {
                let stream_write = stream_read
                    .try_clone()
                    .expect("Unable to obtain write stream");
                break (stream_read, stream_write);
            }
        };

        Self {
            client: Client::new(|| stream_read, || stream_write),
        }
    }
}

impl<M: Message> Deref for IpcRenderer<M> {
    type Target = Client<M>;

    fn deref(&self) -> &Self::Target {
        &self.client
    }
}
