#![cfg(feature = "webrtc")]
use xbox_solo_audio::oferta::{generar_oferta, redactar_sdp};

#[tokio::test]
async fn oferta_minima_con_video_inactive_y_puerto_nueve() {
    let oferta = generar_oferta().await.unwrap();
    let canales: Vec<(&str, &str, bool)> = oferta.canales.iter()
        .map(|c| (c.nombre.as_str(), c.protocolo.as_str(), c.ordenado)).collect();
    assert_eq!(canales, vec![
        ("input", "1.0", true), ("chat", "chatV1", true),
        ("control", "controlV1", true), ("message", "messageV1", true),
    ]);

    let secciones: Vec<&str> = oferta.sdp.split("\r\nm=").collect();
    assert_eq!(secciones.len(), 4, "audio, vídeo y application, en ese orden");
    let audio = secciones[1];
    let video = secciones[2];
    let datos = secciones[3];
    assert!(audio.starts_with("audio 9 UDP/TLS/RTP/SAVPF 111"), "{audio}");
    assert!(audio.contains("a=rtpmap:111 opus/48000/2"));
    assert!(audio.contains("a=fmtp:111 minptime=10;useinbandfec=1;stereo=1"));
    assert!(audio.contains("a=sendrecv"));
    assert!(video.starts_with("video 9 UDP/TLS/RTP/SAVPF 115"), "{video}");
    assert!(video.contains("a=fmtp:115 level-asymmetry-allowed=1;packetization-mode=0;profile-level-id=42e01f"));
    assert!(video.contains("a=inactive"));
    assert!(datos.starts_with("application 9 UDP/DTLS/SCTP webrtc-datachannel"), "{datos}");
    assert!(datos.contains("a=sctp-port:5000"));
    assert!(!oferta.sdp.contains("m=video 0"));
}

#[tokio::test]
async fn salida_publica_oculta_credenciales_del_sdp() {
    let oferta = generar_oferta().await.unwrap();
    let visible = redactar_sdp(&oferta.sdp);
    for prohibido in ["a=ice-ufrag:", "a=ice-pwd:", "a=fingerprint:", "a=candidate:", "o=", "c="] {
        assert!(!visible.lines().any(|linea| linea.starts_with(prohibido)), "se filtró {prohibido}");
    }
    assert!(visible.contains("m=video 9"));
    assert!(visible.contains("a=inactive"));
}
