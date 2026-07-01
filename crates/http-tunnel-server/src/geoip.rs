use anyhow::Context;
use flate2::read::GzDecoder;
use maxminddb::{geoip2, Reader};
use serde::Deserialize;
use std::{
    fs,
    io::Read,
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

const CITY_DB_NAME: &str = "GeoLite2-City.mmdb";
const COUNTRY_DB_NAME: &str = "GeoIP-Country.mmdb";
const EMBEDDED_COUNTRY_DB_GZ: &[u8] = include_bytes!(env!("HTTP_TUNNEL_EMBEDDED_GEOIP_COUNTRY_GZ"));

#[derive(Debug, Clone)]
pub struct CountryLocation {
    pub country_code: String,
    pub country: Option<String>,
}

#[derive(Debug, Deserialize)]
struct IpInfoCountry {
    country: Option<String>,
    country_name: Option<String>,
    continent: Option<String>,
    continent_name: Option<String>,
}

#[derive(Default)]
struct GeoIpCache {
    path: Option<PathBuf>,
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

static GEOIP_CACHE: OnceLock<Mutex<GeoIpCache>> = OnceLock::new();

pub fn ensure_embedded_country_db(data_dir: &str) -> anyhow::Result<bool> {
    if EMBEDDED_COUNTRY_DB_GZ.is_empty() {
        return Ok(false);
    }
    let path = Path::new(data_dir).join(COUNTRY_DB_NAME);
    if path.exists() {
        return Ok(false);
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("create GeoIP data dir {}", parent.display()))?;
    }

    let mut decoder = GzDecoder::new(EMBEDDED_COUNTRY_DB_GZ);
    let mut bytes = Vec::new();
    decoder
        .read_to_end(&mut bytes)
        .context("decompress embedded GeoIP country database")?;
    let tmp_path = path.with_extension("mmdb.tmp");
    fs::write(&tmp_path, bytes)
        .with_context(|| format!("write GeoIP database {}", tmp_path.display()))?;
    fs::rename(&tmp_path, &path).with_context(|| {
        format!(
            "install GeoIP database {} -> {}",
            tmp_path.display(),
            path.display()
        )
    })?;
    Ok(true)
}

pub fn lookup_country(data_dir: &str, ip: IpAddr) -> Option<CountryLocation> {
    lookup_country_path(&Path::new(data_dir).join(COUNTRY_DB_NAME), ip)
        .or_else(|| lookup_country_path(&Path::new(data_dir).join(CITY_DB_NAME), ip))
}

fn lookup_country_path(path: &Path, ip: IpAddr) -> Option<CountryLocation> {
    let reader = reader_for(path)?;
    let lookup = reader.lookup(ip).ok()?;
    if !lookup.has_data() {
        return None;
    }

    if let Some(record) = lookup.decode::<IpInfoCountry>().ok().flatten() {
        if let Some(code) = normalize_country_code(record.country.as_deref()) {
            return Some(CountryLocation {
                country_code: code,
                country: record.country_name.or(record.continent_name),
            });
        }
        if let Some(code) = normalize_country_code(record.continent.as_deref()) {
            return Some(CountryLocation {
                country_code: code,
                country: record.continent_name,
            });
        }
    }

    if let Some(country) = lookup.decode::<geoip2::Country<'_>>().ok().flatten() {
        if let Some(code) = normalize_country_code(country.country.iso_code) {
            return Some(CountryLocation {
                country_code: code,
                country: name(country.country.names),
            });
        }
        if let Some(code) = normalize_country_code(country.registered_country.iso_code) {
            return Some(CountryLocation {
                country_code: code,
                country: name(country.registered_country.names),
            });
        }
    }

    if let Some(city) = lookup.decode::<geoip2::City<'_>>().ok().flatten() {
        if let Some(code) = normalize_country_code(city.country.iso_code) {
            return Some(CountryLocation {
                country_code: code,
                country: name(city.country.names),
            });
        }
    }

    None
}

fn reader_for(path: &Path) -> Option<Arc<Reader<Vec<u8>>>> {
    let cache = GEOIP_CACHE.get_or_init(|| Mutex::new(GeoIpCache::default()));
    let mut cache = cache.lock().ok()?;
    if cache.path.as_deref() == Some(path) {
        if cache.reader.is_some() || !path.exists() {
            return cache.reader.clone();
        }
    }

    let reader = Reader::open_readfile(path).ok().map(Arc::new);
    cache.path = Some(path.to_path_buf());
    cache.reader = reader.clone();
    reader
}

fn name(names: maxminddb::geoip2::Names<'_>) -> Option<String> {
    names
        .simplified_chinese
        .or(names.english)
        .map(ToString::to_string)
}

pub fn normalize_country_code(value: Option<&str>) -> Option<String> {
    let code = value?.trim();
    if code.len() != 2 || !code.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    let code = code.to_ascii_uppercase();
    if code == "XX" {
        return None;
    }
    Some(code)
}
