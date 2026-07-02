pub fn normalize_country_code(value: &str) -> Option<String> {
    let code = value.trim();
    if code.len() != 2 || !code.chars().all(|ch| ch.is_ascii_alphabetic()) {
        return None;
    }
    let code = code.to_ascii_uppercase();
    if code == "XX" {
        return None;
    }
    Some(code)
}

pub fn country_code_from_name(value: &str) -> Option<&'static str> {
    let value = normalize_country_name(value)?;
    COUNTRY_NAME_CODES
        .iter()
        .find_map(|(name, code)| (*name == value).then_some(*code))
}

pub fn country_from_location(value: &str) -> Option<(&'static str, String)> {
    let country = value
        .split(['-', '–', '—'])
        .next()
        .map(str::trim)
        .filter(|value| !value.is_empty())?;
    let code = country_code_from_name(&country)?;
    Some((code, country.to_string()))
}

fn normalize_country_name(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|ch: char| ch.is_ascii_punctuation() || ch.is_whitespace())
        .trim();
    (!value.is_empty()).then(|| value.to_ascii_lowercase())
}

const COUNTRY_NAME_CODES: &[(&str, &str)] = &[
    ("中国", "CN"),
    ("china", "CN"),
    ("cn", "CN"),
    ("中国香港", "HK"),
    ("香港", "HK"),
    ("hong kong", "HK"),
    ("中国澳门", "MO"),
    ("澳门", "MO"),
    ("macau", "MO"),
    ("macao", "MO"),
    ("中国台湾", "TW"),
    ("台湾", "TW"),
    ("taiwan", "TW"),
    ("美国", "US"),
    ("美國", "US"),
    ("united states", "US"),
    ("usa", "US"),
    ("us", "US"),
    ("日本", "JP"),
    ("japan", "JP"),
    ("韩国", "KR"),
    ("南韩", "KR"),
    ("south korea", "KR"),
    ("新加坡", "SG"),
    ("singapore", "SG"),
    ("德国", "DE"),
    ("germany", "DE"),
    ("英国", "GB"),
    ("united kingdom", "GB"),
    ("great britain", "GB"),
    ("法国", "FR"),
    ("france", "FR"),
    ("荷兰", "NL"),
    ("netherlands", "NL"),
    ("加拿大", "CA"),
    ("canada", "CA"),
    ("澳大利亚", "AU"),
    ("澳洲", "AU"),
    ("australia", "AU"),
    ("俄罗斯", "RU"),
    ("russia", "RU"),
    ("印度", "IN"),
    ("india", "IN"),
    ("巴西", "BR"),
    ("brazil", "BR"),
    ("印尼", "ID"),
    ("印度尼西亚", "ID"),
    ("indonesia", "ID"),
    ("泰国", "TH"),
    ("thailand", "TH"),
    ("越南", "VN"),
    ("vietnam", "VN"),
    ("马来西亚", "MY"),
    ("malaysia", "MY"),
    ("菲律宾", "PH"),
    ("philippines", "PH"),
    ("土耳其", "TR"),
    ("turkey", "TR"),
    ("阿联酋", "AE"),
    ("阿拉伯联合酋长国", "AE"),
    ("united arab emirates", "AE"),
    ("沙特阿拉伯", "SA"),
    ("saudi arabia", "SA"),
    ("意大利", "IT"),
    ("italy", "IT"),
    ("西班牙", "ES"),
    ("spain", "ES"),
    ("瑞士", "CH"),
    ("switzerland", "CH"),
    ("瑞典", "SE"),
    ("sweden", "SE"),
    ("挪威", "NO"),
    ("norway", "NO"),
    ("芬兰", "FI"),
    ("finland", "FI"),
    ("丹麦", "DK"),
    ("denmark", "DK"),
    ("波兰", "PL"),
    ("poland", "PL"),
    ("爱尔兰", "IE"),
    ("ireland", "IE"),
    ("奥地利", "AT"),
    ("austria", "AT"),
    ("比利时", "BE"),
    ("belgium", "BE"),
    ("葡萄牙", "PT"),
    ("portugal", "PT"),
    ("捷克", "CZ"),
    ("czechia", "CZ"),
    ("czech republic", "CZ"),
    ("乌克兰", "UA"),
    ("ukraine", "UA"),
    ("墨西哥", "MX"),
    ("mexico", "MX"),
    ("阿根廷", "AR"),
    ("argentina", "AR"),
    ("智利", "CL"),
    ("chile", "CL"),
    ("哥伦比亚", "CO"),
    ("colombia", "CO"),
    ("秘鲁", "PE"),
    ("peru", "PE"),
    ("南非", "ZA"),
    ("south africa", "ZA"),
    ("埃及", "EG"),
    ("egypt", "EG"),
    ("尼日利亚", "NG"),
    ("nigeria", "NG"),
    ("肯尼亚", "KE"),
    ("kenya", "KE"),
    ("以色列", "IL"),
    ("israel", "IL"),
    ("巴基斯坦", "PK"),
    ("pakistan", "PK"),
    ("孟加拉国", "BD"),
    ("bangladesh", "BD"),
    ("斯里兰卡", "LK"),
    ("sri lanka", "LK"),
    ("新西兰", "NZ"),
    ("new zealand", "NZ"),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_country_codes() {
        assert_eq!(normalize_country_code(" cn "), Some("CN".to_string()));
        assert_eq!(normalize_country_code("XX"), None);
        assert_eq!(normalize_country_code("china"), None);
    }

    #[test]
    fn maps_country_names_and_locations() {
        assert_eq!(country_code_from_name("中国"), Some("CN"));
        assert_eq!(country_code_from_name("United States"), Some("US"));
        assert_eq!(
            country_from_location("中国–上海–上海 移动"),
            Some(("CN", "中国".to_string()))
        );
    }
}
