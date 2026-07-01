use maxminddb::{geoip2, Reader};
use std::{
    net::IpAddr,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, OnceLock},
};

const CITY_DB_NAME: &str = "GeoLite2-City.mmdb";

#[derive(Debug, Clone)]
pub struct GeoLocation {
    pub country_code: Option<String>,
    pub country: Option<String>,
    pub region: Option<String>,
    pub city: Option<String>,
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Default)]
struct GeoIpCache {
    path: Option<PathBuf>,
    reader: Option<Arc<Reader<Vec<u8>>>>,
}

static GEOIP_CACHE: OnceLock<Mutex<GeoIpCache>> = OnceLock::new();

pub fn lookup(data_dir: &str, ip: IpAddr) -> Option<GeoLocation> {
    let reader = reader_for(&Path::new(data_dir).join(CITY_DB_NAME))?;
    let lookup = reader.lookup(ip).ok()?;
    if !lookup.has_data() {
        return None;
    }
    let city = lookup.decode::<geoip2::City<'_>>().ok()??;
    let latitude = city.location.latitude?;
    let longitude = city.location.longitude?;
    Some(GeoLocation {
        country_code: city.country.iso_code.map(ToString::to_string),
        country: name(city.country.names),
        region: city
            .subdivisions
            .first()
            .and_then(|subdivision| name(subdivision.names.clone())),
        city: name(city.city.names),
        latitude: coarse_coordinate(latitude, -90.0, 90.0),
        longitude: coarse_coordinate(longitude, -180.0, 180.0),
    })
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

fn coarse_coordinate(value: f64, min: f64, max: f64) -> f64 {
    ((value.clamp(min, max) * 10.0).round()) / 10.0
}
