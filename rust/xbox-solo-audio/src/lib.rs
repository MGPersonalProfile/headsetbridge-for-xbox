// sin WebRTC, código muerto = error (un experimento que se cierra
// se quita entero; lo público sin uso lo vigila tools/guardia.mjs).
#![cfg_attr(not(feature = "webrtc"), deny(dead_code, unused_imports))]

// sin TLS el HTTPS no anda; que no compile en vez de fallar al conectar.
#[cfg(not(any(feature = "rustls", feature = "schannel", feature = "winhttp")))]
compile_error!("hace falta un HTTPS: la feature `schannel` (la de por defecto), `rustls` o `winhttp` (`--no-default-features --features …`)");
// las dos TLS de reqwest a la vez no: no se sabría cuál se usa.
#[cfg(all(feature = "rustls", feature = "schannel", not(feature = "winhttp")))]
compile_error!("`rustls` y `schannel` a la vez: para rustls, `--no-default-features --features rustls`");
pub mod audio;
pub mod auth;
#[cfg(feature = "webrtc")]
pub mod canales;
pub mod depurar;
pub mod elastico;
pub mod http;
#[cfg(windows)]
pub mod iac3;
pub mod micro;
pub mod raton_teclado;
pub mod modo;
pub mod nano;
#[cfg(feature = "webrtc")]
pub mod oferta;
pub mod red;
pub mod registro;
pub mod reintento;
pub mod relevo;
pub mod reserva;
#[cfg(feature = "webrtc")]
pub mod sesion;
pub mod medida;
pub mod volcado;
pub mod xhome;
