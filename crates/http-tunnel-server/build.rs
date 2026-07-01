use std::{env, fs, path::PathBuf};

fn main() {
    println!("cargo:rerun-if-env-changed=HTTP_TUNNEL_EMBED_GEOIP_COUNTRY_GZ");
    println!("cargo:rerun-if-changed=assets/GeoIP-Country.mmdb.gz");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set"));
    let embedded = out_dir.join("embedded-geoip-country.mmdb.gz");
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let default_asset = manifest_dir.join("assets/GeoIP-Country.mmdb.gz");
    let source = match env::var_os("HTTP_TUNNEL_EMBED_GEOIP_COUNTRY_GZ") {
        Some(path) => {
            let path = PathBuf::from(path);
            if !path.exists() {
                panic!(
                    "HTTP_TUNNEL_EMBED_GEOIP_COUNTRY_GZ does not exist: {}",
                    path.display()
                );
            }
            Some(path)
        }
        None => default_asset.exists().then_some(default_asset),
    };

    if let Some(source) = source {
        fs::copy(&source, &embedded).expect("copy embedded GeoIP country database");
    } else {
        fs::write(&embedded, []).expect("write empty embedded GeoIP country placeholder");
    }

    println!(
        "cargo:rustc-env=HTTP_TUNNEL_EMBEDDED_GEOIP_COUNTRY_GZ={}",
        embedded.display()
    );
}
