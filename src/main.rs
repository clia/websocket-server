//! Simple echo websocket server.
//! Open `http://localhost:8080/ws/index.html` in browser

use std::{cell::RefCell, io, rc::Rc, time::Duration, time::Instant};
use std::fs::File;
use std::io::BufReader;

// use ntex_files::Files;
use rustls::{Certificate, PrivateKey, ServerConfig};
use rustls_pemfile::{certs, rsa_private_keys};

use futures::future::{ready, select, Either};
use ntex::service::{fn_factory_with_config, fn_service, Service};
use ntex::web::{self, middleware, ws, App, Error, HttpRequest, HttpResponse};
use ntex::{channel::oneshot, rt, time, util::Bytes};
use ntex_files as fs;

/// How often heartbeat pings are sent
const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(5);
/// How long before lack of client response causes a timeout
const CLIENT_TIMEOUT: Duration = Duration::from_secs(10);

struct WsState {
    /// Client must send ping at least once per 10 seconds (CLIENT_TIMEOUT),
    /// otherwise we drop connection.
    hb: Instant,
}

/// WebSockets service factory
async fn ws_service(
    sink: ws::WsSink,
) -> Result<
    impl Service<ws::Frame, Response = Option<ws::Message>, Error = io::Error>,
    web::Error,
> {
    let state = Rc::new(RefCell::new(WsState { hb: Instant::now() }));

    // disconnect notification
    let (tx, rx) = oneshot::channel();

    // start heartbeat task
    rt::spawn(heartbeat(state.clone(), sink, rx));

    // websockets handler service
    Ok(fn_service(move |frame| {
        println!("WS Frame: {:?}", frame);

        let item = match frame {
            ws::Frame::Ping(msg) => {
                (*state.borrow_mut()).hb = Instant::now();
                ws::Message::Pong(msg)
            }
            ws::Frame::Text(text) => ws::Message::Text(
                String::from_utf8(Vec::from(text.as_ref())).unwrap().into(),
            ),
            ws::Frame::Binary(bin) => ws::Message::Binary(bin),
            ws::Frame::Close(reason) => ws::Message::Close(reason),
            _ => ws::Message::Close(None),
        };
        ready(Ok(Some(item)))
    })
    // on_shutdown callback is being called when service get shutdowned by dispatcher
    // in this case when connection get dropped
    .on_shutdown(move || {
        let _ = tx.send(());
    }))
}

/// helper method that sends ping to client every heartbeat interval
async fn heartbeat(
    state: Rc<RefCell<WsState>>,
    sink: ws::WsSink,
    mut rx: oneshot::Receiver<()>,
) {
    loop {
        match select(Box::pin(time::sleep(HEARTBEAT_INTERVAL)), &mut rx).await {
            Either::Left(_) => {
                // check client heartbeats
                if Instant::now().duration_since(state.borrow().hb) > CLIENT_TIMEOUT {
                    // heartbeat timed out
                    println!("Websocket Client heartbeat failed, disconnecting!");
                    return;
                }

                // send ping
                if sink.send(ws::Message::Ping(Bytes::new())).await.is_err() {
                    return;
                }
            }
            Either::Right(_) => {
                println!("Connection is dropped, stop heartbeat task");
                return;
            }
        }
    }
}

/// do websocket handshake and start web sockets service
async fn ws_index(req: HttpRequest) -> Result<HttpResponse, Error> {
    ws::start(req, fn_factory_with_config(ws_service)).await
}

#[ntex::main]
async fn main() -> std::io::Result<()> {
    std::env::set_var("RUST_LOG", "ntex=trace");
    env_logger::init();

    // load ssl keys
    let key_file = &mut BufReader::new(File::open("key.pem").unwrap());
    let key = PrivateKey(rsa_private_keys(key_file).unwrap().remove(0));
    let cert_file = &mut BufReader::new(File::open("cert.pem").unwrap());
    let cert_chain = certs(cert_file)
        .unwrap()
        .iter()
        .map(|c| Certificate(c.to_vec()))
        .collect();
    let config = ServerConfig::builder()
        .with_safe_defaults()
        .with_no_client_auth()
        .with_single_cert(cert_chain, key)
        .unwrap();

    web::server(|| {
        App::new()
            // enable logger
            .wrap(middleware::Logger::default())
            // websocket route
            .service(web::resource("/ws").route(web::get().to(ws_index)))
            // static files
            .service(fs::Files::new("/", "./").index_file("index.html").show_files_listing())
            // .service(Files::new("/static", "static"))
    })
    // start http server on 127.0.0.1:8080
    .bind("0.0.0.0:80")?
    .bind_rustls("0.0.0.0:443", config)?
    .run()
    .await
}
