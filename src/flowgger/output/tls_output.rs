use flowgger::config::Config;
use flowgger::merger::Merger;
use openssl::bn::BigNum;
use openssl::dh::DH;
use openssl::ssl::*;
use openssl::x509::X509FileType;
use std::error::Error;
use std::io;
use std::io::{stderr, BufWriter, ErrorKind, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use super::Output;

const DEFAULT_CIPHERS: &'static str = "ECDHE-ECDSA-AES128-GCM-SHA256:ECDHE-RSA-AES128-GCM-SHA256:ECDHE-ECDSA-CHACHA20-POLY1305:ECDHE-RSA-CHACHA20-POLY1305:ECDHE-ECDSA-AES128-SHA256:ECDHE-RSA-AES128-SHA256:ECDHE-ECDSA-AES128-SHA:ECDHE-RSA-AES128-SHA:ECDHE-ECDSA-AES256-GCM-SHA384:ECDHE-RSA-AES256-GCM-SHA384:ECDHE-ECDSA-AES256-SHA384:ECDHE-RSA-AES256-SHA384:ECDHE-ECDSA-AES256-SHA:ECDHE-RSA-AES256-SHA:AES128-GCM-SHA256:AES256-GCM-SHA384:AES128-SHA256:AES256-SHA256:AES128-SHA:AES256-SHA:ECDHE-ECDSA-DES-CBC3-SHA:ECDHE-RSA-DES-CBC3-SHA:DES-CBC3-SHA:!aNULL:!eNULL:!EXPORT:!DES:!RC4:!MD5:!PSK:!aECDH:!EDH-DSS-DES-CBC3-SHA:!EDH-RSA-DES-CBC3-SHA:!KRB5-DES-CBC3-SHA";
const DEFAULT_CONNECT: &'static str = "127.0.0.1:6514";
const DEFAULT_SLEEP_AFTER_CONNECTION_FAILURE: u32 = 1000;
const TLS_DEFAULT_TIMEOUT: u64 = 3600;
const TLS_DEFAULT_THREADS: u32 = 1;

pub struct TlsOutput {
    config: TlsConfig,
    threads: u32
}

#[derive(Clone)]
struct TlsConfig {
    timeout: Option<Duration>,
    connect: String
}

struct TlsWorker {
    arx: Arc<Mutex<Receiver<Vec<u8>>>>,
    merger: Option<Box<Merger + Send>>,
    config: TlsConfig
}

impl TlsWorker {
    fn new(arx: Arc<Mutex<Receiver<Vec<u8>>>>, merger: Option<Box<Merger + Send>>, config: TlsConfig) -> TlsWorker {
        TlsWorker {
            arx: arx,
            merger: merger,
            config: config
        }
    }

    fn handle_connection(&self, connect: &str) -> io::Result<()> {
        let client = try!(new_tcp(connect));
        let _ = writeln!(stderr(), "Connected to {}", connect);
        let tls_method = SslMethod::Sslv23;
        let mut ctx = SslContext::new(tls_method).unwrap();
        let ciphers = DEFAULT_CIPHERS;
        set_fs(&mut ctx);
        ctx.set_cipher_list(&ciphers).unwrap();
        let sslclient = match SslStream::connect(&ctx, client) {
            Err(_) => return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "SSL handshake aborted by the server")),
            Ok(sslclient) => sslclient
        };
        let _ = writeln!(stderr(), "Completed SSL handshake with {}", connect);
        let mut writer = BufWriter::new(sslclient);
        let merger = &self.merger;
        loop {
            let mut bytes = match { self.arx.lock().unwrap().recv() } {
                Ok(line) => line,
                Err(_) => return Err(io::Error::new(io::ErrorKind::Other, "Cannot read the message queue any more"))
            };
            if let Some(ref merger) = *merger {
                merger.frame(&mut bytes);
            }
            match writer.write_all(&bytes) {
                Ok(_) => { },
                Err(e) => match e.kind() {
                    ErrorKind::Interrupted => continue,
                    _ => return Err(e)
                }
            };
        }
    }

    fn run(self) {
        loop {
            let ref connect = self.config.connect;
            if let Err(e) = self.handle_connection(&connect) {
                match e.kind() {
                    ErrorKind::ConnectionRefused => {
                        let _ = writeln!(stderr(), "Connection to {} refused", connect);
                    }
                    ErrorKind::ConnectionAborted | ErrorKind::ConnectionReset => {
                        let _ = writeln!(stderr(), "Connection to {} aborted by the server", connect);
                    }
                    _ => {
                        let _ = writeln!(stderr(), "Error while communicating with {} - {}", connect, e);
                    }
                }
            }
            thread::sleep_ms(DEFAULT_SLEEP_AFTER_CONNECTION_FAILURE);
            let _ = writeln!(stderr(), "Attempting to reconnect");
        }
    }
}

fn new_tcp(connect: &str) -> Result<TcpStream, io::Error> {
    loop {
        match TcpStream::connect(connect) {
            Ok(stream) => return Ok(stream),
            Err(e) => {
                return Err(e);
            }
        }
    }
}

impl TlsOutput {
    pub fn new(config: &Config) -> TlsOutput {
        let connect = config.lookup("output.connect").map_or(DEFAULT_CONNECT, |x|x.as_str().
            expect("input.listen must be an ip:port string")).to_owned();
        let timeout = config.lookup("output.tls_timeout").
            map_or(TLS_DEFAULT_TIMEOUT, |x| x.as_integer().
                expect("output.tls_timeout must be an unsigned integer") as u64);
        let threads = config.lookup("output.tls_threads").
            map_or(TLS_DEFAULT_THREADS, |x| x.as_integer().
                expect("output.tls_threads must be a 32-bit integer") as u32);
        let tls_config = TlsConfig {
            connect: connect,
            timeout: Some(Duration::from_secs(timeout))
        };
        TlsOutput {
            config: tls_config,
            threads: threads
        }
    }
}

impl Output for TlsOutput {
    fn start(&self, arx: Arc<Mutex<Receiver<Vec<u8>>>>, merger: Option<Box<Merger>>) {
        for _ in 0..self.threads {
            let arx = arx.clone();
            let config = self.config.clone();
            let merger = match merger {
                Some(ref merger) => Some(merger.clone_boxed()) as Option<Box<Merger + Send>>,
                None => None
            };
            thread::spawn(move || {
                let worker = TlsWorker::new(arx, merger, config);
                worker.run();
            });
        }
    }
}

#[cfg(feature = "ecdh")]
fn set_ecdh(ctx: &mut SslContext) {
    ctx.set_ecdh_auto(true).unwrap();
}

#[cfg(not(feature = "ecdh"))]
fn set_ecdh(ctx: &mut SslContext) {
    let _ = ctx;
}

fn set_fs(ctx: &mut SslContext) {
    let p = BigNum::from_hex_str("87A8E61DB4B6663CFFBBD19C651959998CEEF608660DD0F25D2CEED4435E3B00E00DF8F1D61957D4FAF7DF4561B2AA3016C3D91134096FAA3BF4296D830E9A7C209E0C6497517ABD5A8A9D306BCF67ED91F9E6725B4758C022E0B1EF4275BF7B6C5BFC11D45F9088B941F54EB1E59BB8BC39A0BF12307F5C4FDB70C581B23F76B63ACAE1CAA6B7902D52526735488A0EF13C6D9A51BFA4AB3AD8347796524D8EF6A167B5A41825D967E144E5140564251CCACB83E6B486F6B3CA3F7971506026C0B857F689962856DED4010ABD0BE621C3A3960A54E710C375F26375D7014103A4B54330C198AF126116D2276E11715F693877FAD7EF09CADB094AE91E1A1597").unwrap();
    let g = BigNum::from_hex_str("3FB32C9B73134D0B2E77506660EDBD484CA7B18F21EF205407F4793A1A0BA12510DBC15077BE463FFF4FED4AAC0BB555BE3A6C1B0C6B47B1BC3773BF7E8C6F62901228F8C28CBB18A55AE31341000A650196F931C77A57F2DDF463E5E9EC144B777DE62AAAB8A8628AC376D282D6ED3864E67982428EBC831D14348F6F2F9193B5045AF2767164E1DFC967C1FB3F2E55A4BD1BFFE83B9C80D052B985D182EA0ADB2A3B7313D3FE14C8484B1E052588B9B7D2BBD2DF016199ECD06E1557CD0915B3353BBB64E0EC377FD028370DF92B52C7891428CDC67EB6184B523D1DB246C32F63078490F00EF8D647D148D47954515E2327CFEF98C582664B4C0F6CC41659").unwrap();
    let q = BigNum::from_hex_str("8CF83642A709A097B447997640129DA299B1A47D1EB3750BA308B0FE64F5FBD3").unwrap();
    let dh = DH::from_params(p, g, q).unwrap();
    ctx.set_tmp_dh(dh).unwrap();
    set_ecdh(ctx);
}