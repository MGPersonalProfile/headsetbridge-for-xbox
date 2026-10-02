#![cfg(feature = "webrtc")]
use xbox_solo_audio::canales::{
    cambio_mando, configuracion, handshake, paquete_mando, paquete_metadatos,
    solicitud_autorizacion, TramaMando, CANALES,
};

const MANIFEST: &str = include_str!("../../vectores/manifest.json");

#[test]
fn orden_y_protocolos_coinciden_con_node() {
    let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    let real: Vec<Vec<&str>> = CANALES.iter().map(|c| vec![c.nombre, c.protocolo]).collect();
    assert_eq!(serde_json::to_value(real).unwrap(), manifest["ordenCanales"]);
}

#[test]
fn mensajes_json_son_los_mismos_bytes_utf8_que_node() {
    let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    let ids: Vec<&str> = manifest["ids"].as_array().unwrap().iter().map(|v| v.as_str().unwrap()).collect();
    assert_eq!(handshake(ids[0]), include_bytes!("../../vectores/handshake.bin"));
    assert_eq!(solicitud_autorizacion(), include_bytes!("../../vectores/autorizacion.bin"));
    assert_eq!(cambio_mando(), include_bytes!("../../vectores/cambioMando.bin"));
    let esperados: [&[u8]; 6] = [
        include_bytes!("../../vectores/configuracion0.bin"),
        include_bytes!("../../vectores/configuracion1.bin"),
        include_bytes!("../../vectores/configuracion2.bin"),
        include_bytes!("../../vectores/configuracion3.bin"),
        include_bytes!("../../vectores/configuracion4.bin"),
        include_bytes!("../../vectores/configuracion5.bin"),
    ];
    for (indice, esperado) in esperados.into_iter().enumerate() {
        let id = ids[if indice == 0 { 1 } else { indice + 2 }];
        assert_eq!(configuracion(indice, id, ids[2]).unwrap(), esperado, "configuración {indice}");
    }
    assert!(configuracion(6, ids[0], ids[2]).is_none());
}

#[test]
fn paquetes_input_tienen_cabecera_de_catorce_bytes() {
    let manifest: serde_json::Value = serde_json::from_str(MANIFEST).unwrap();
    let tiempo = manifest["tiempoMs"].as_f64().unwrap();
    let metadatos = paquete_metadatos(0, tiempo, 2);
    assert_eq!(metadatos.len(), 15);
    assert_eq!(metadatos, include_bytes!("../../vectores/metadatos.bin"));

    let reposo = paquete_mando(42, tiempo, &[TramaMando::default()]);
    assert_eq!(reposo.len(), 14 + 1 + 23);
    assert_eq!(reposo, include_bytes!("../../vectores/mandoReposo.bin"));

    let pulso = paquete_mando(1, tiempo, &[TramaMando { lx: 0.1, ..Default::default() }, TramaMando::default()]);
    assert_eq!(pulso.len(), 14 + 1 + 23 * 2);
    assert_eq!(pulso, include_bytes!("../../vectores/pulsoInactividad.bin"));

    let dos = paquete_mando(65535, tiempo, &[TramaMando { lx: 0.1, ly: -0.5 }, TramaMando::default()]);
    assert_eq!(dos, include_bytes!("../../vectores/mandoDosTramas.bin"));
    assert_eq!(&dos[0..2], &[2, 0]); // Xcloud-rs omite ese segundo byte.
    assert_eq!(&dos[2..6], &65535u32.to_le_bytes());
    assert_eq!(&dos[6..14], &tiempo.to_le_bytes());
}

#[cfg(feature = "webrtc")]
#[test]
fn red_rfc2198_un_bloque_redundante() {
    // Bloque redundante PT 111, retroceso 960 (un paquete), 3 bytes; principal PT 111, 2 bytes.
    let carga = [0x80 | 111, 0x0F, 0x00, 0x03, 111, 0xAA, 0xBB, 0xCC, 0x11, 0x22];
    let bloques = xbox_solo_audio::sesion::desempaquetar_red(&carga).unwrap();
    assert_eq!(bloques, vec![(1, &[0xAA, 0xBB, 0xCC][..]), (0, &[0x11, 0x22][..])]);
}

#[cfg(feature = "webrtc")]
#[test]
fn red_sin_redundancia_y_mal_formado() {
    assert_eq!(xbox_solo_audio::sesion::desempaquetar_red(&[111, 0x01]).unwrap(), vec![(0, &[0x01][..])]);
    assert!(xbox_solo_audio::sesion::desempaquetar_red(&[0x80 | 111, 0x0F, 0x00, 0x09, 111, 0xAA]).is_none());
}
