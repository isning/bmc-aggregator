use md5::{Digest as Md5Digest, Md5};
use rand::Rng;
use std::collections::HashMap;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DigestParams {
    pub realm: String,
    pub nonce: String,
    /// Selected supported qop, rather than the server's list of alternatives.
    pub qop: Option<String>,
    pub opaque: Option<String>,
}

fn token(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// Parse one Digest challenge. Unsupported algorithms and ambiguous fields fail closed.
pub fn parse_digest_header(header: &str) -> Option<DigestParams> {
    let (scheme, mut rest) = header.trim().split_once(char::is_whitespace)?;
    if !scheme.eq_ignore_ascii_case("Digest") || header.chars().any(|c| c.is_control() && c != '\t')
    {
        return None;
    }
    let mut fields = HashMap::new();
    loop {
        rest = rest.trim_start();
        let (name, value) = rest.split_once('=')?;
        let name = name.trim().to_ascii_lowercase();
        if !token(&name) {
            return None;
        }
        rest = value.trim_start();
        let parsed;
        if let Some(quoted) = rest.strip_prefix('"') {
            let mut output = String::new();
            let mut escaped = false;
            let mut end = None;
            for (index, character) in quoted.char_indices() {
                if escaped {
                    output.push(character);
                    escaped = false;
                } else {
                    match character {
                        '\\' => escaped = true,
                        '"' => {
                            end = Some(index + 1);
                            break;
                        }
                        _ => output.push(character),
                    }
                }
            }
            rest = &quoted[end?..];
            parsed = output;
        } else {
            let end = rest.find(',').unwrap_or(rest.len());
            let value = rest[..end].trim();
            if !token(value) {
                return None;
            }
            parsed = value.to_owned();
            rest = &rest[end..];
        }
        if fields.insert(name, parsed).is_some() {
            return None;
        }
        rest = rest.trim_start();
        if rest.is_empty() {
            break;
        }
        rest = rest.strip_prefix(',')?;
        if rest.trim().is_empty() {
            return None;
        }
    }
    // This implementation supports MD5 only. Do not silently reinterpret a
    // SHA or session-algorithm challenge as MD5.
    if fields
        .get("algorithm")
        .is_some_and(|value| !value.eq_ignore_ascii_case("MD5"))
        || fields
            .get("charset")
            .is_some_and(|value| !value.eq_ignore_ascii_case("UTF-8"))
        || fields
            .get("userhash")
            .is_some_and(|value| !value.eq_ignore_ascii_case("false"))
    {
        return None;
    }
    let qop = match fields.remove("qop") {
        Some(value) if value.split(',').any(|part| part.trim() == "auth") => Some("auth".into()),
        Some(_) => return None,
        None => None,
    };
    let nonce = fields.remove("nonce")?;
    if nonce.is_empty() {
        return None;
    }
    Some(DigestParams {
        realm: fields.remove("realm")?,
        nonce,
        qop,
        opaque: fields.remove("opaque"),
    })
}

/// Select a supported Digest challenge from a combined WWW-Authenticate field.
/// Commas inside quoted strings never delimit authentication schemes.
pub fn find_digest_challenge(header: &str) -> Option<DigestParams> {
    let mut starts = vec![0];
    let mut quoted = false;
    let mut escaped = false;
    for (index, character) in header.char_indices() {
        if escaped {
            escaped = false;
            continue;
        }
        match character {
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            ',' if !quoted => {
                let next = header[index + 1..].trim_start();
                if let Some((scheme, parameters)) = next.split_once(char::is_whitespace) {
                    if token(scheme) && !parameters.trim_start().starts_with('=') {
                        starts.push(index + 1);
                    }
                }
            }
            _ => {}
        }
    }
    starts.push(header.len());
    starts.windows(2).find_map(|range| {
        parse_digest_header(
            header[range[0]..range[1]]
                .trim()
                .trim_end_matches(',')
                .trim(),
        )
    })
}

fn md5_hex(input: &str) -> String {
    let mut hash = Md5::new();
    hash.update(input.as_bytes());
    format!("{:x}", hash.finalize())
}

fn quote(value: &str) -> Result<String, String> {
    if value.chars().any(char::is_control) {
        return Err("Digest field contains control characters".into());
    }
    Ok(value.replace('\\', "\\\\").replace('"', "\\\""))
}

pub fn build_digest_auth(
    username: &str,
    password: &str,
    params: &DigestParams,
    uri: &str,
    method: &str,
) -> Result<String, String> {
    let client_nonce: String = rand::thread_rng()
        .sample_iter(&rand::distributions::Alphanumeric)
        .take(32)
        .map(char::from)
        .collect();
    build_with_nonce(username, password, params, uri, method, &client_nonce)
}

fn build_with_nonce(
    username: &str,
    password: &str,
    params: &DigestParams,
    uri: &str,
    method: &str,
    client_nonce: &str,
) -> Result<String, String> {
    if params.nonce.is_empty()
        || !token(method)
        || params.qop.as_deref().is_some_and(|qop| qop != "auth")
    {
        return Err("Unsupported or invalid Digest parameters".into());
    }
    let ha1 = md5_hex(&format!("{}:{}:{}", username, params.realm, password));
    let ha2 = md5_hex(&format!("{method}:{uri}"));
    let response = match &params.qop {
        Some(qop) => md5_hex(&format!(
            "{ha1}:{}:00000001:{client_nonce}:{qop}:{ha2}",
            params.nonce
        )),
        None => md5_hex(&format!("{ha1}:{}:{ha2}", params.nonce)),
    };
    let mut authorization = format!(
        "Digest username=\"{}\", realm=\"{}\", nonce=\"{}\", uri=\"{}\", response=\"{response}\", algorithm=MD5",
        quote(username)?, quote(&params.realm)?, quote(&params.nonce)?, quote(uri)?,
    );
    if params.qop.is_some() {
        authorization.push_str(&format!(
            ", qop=auth, nc=00000001, cnonce=\"{}\"",
            quote(client_nonce)?
        ));
    }
    if let Some(opaque) = &params.opaque {
        authorization.push_str(&format!(", opaque=\"{}\"", quote(opaque)?));
    }
    Ok(authorization)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combined_challenges_and_mixed_case_schemes_are_selected_safely() {
        for header in [
            r#"Basic realm="fake,Digest bait", dIgEsT realm="real",nonce="n",qop="auth-int,auth""#,
            r#"Digest realm="real",nonce="n",algorithm=MD5, Basic realm="other""#,
            r#"Digest realm="unsupported",nonce="n",algorithm=SHA-256, Digest realm="real",nonce="n""#,
        ] {
            assert_eq!(find_digest_challenge(header).unwrap().realm, "real");
        }
        assert!(find_digest_challenge(r#"Basic realm="Digest realm=real,nonce=n""#).is_none());
    }

    #[test]
    fn rfc2617_known_answer_with_auth_qop() {
        let params = parse_digest_header(
            r#"Digest realm="testrealm@host.com", qop="auth,auth-int", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093", opaque="5ccc069c403ebaf9f0171e9517f40e41""#
        ).unwrap();
        let header = build_with_nonce(
            "Mufasa",
            "Circle Of Life",
            &params,
            "/dir/index.html",
            "GET",
            "0a4f113b",
        )
        .unwrap();
        assert!(header.contains(r#"response="6629fae49393a05397450978507c4ef1""#));
        assert!(header.contains("qop=auth, nc=00000001"));
    }

    #[test]
    fn legacy_digest_omits_qop_fields_and_uses_legacy_formula() {
        let params = parse_digest_header(
            r#"Digest realm="testrealm@host.com", nonce="dcd98b7102dd2f0e8b11d0f600bfb0c093""#,
        )
        .unwrap();
        let header = build_with_nonce(
            "Mufasa",
            "Circle Of Life",
            &params,
            "/dir/index.html",
            "GET",
            "unused",
        )
        .unwrap();
        assert!(header.contains(r#"response="670fd8c2df070c60b045671b8b24ff02""#));
        for field in ["qop=", "nc=", "cnonce="] {
            assert!(!header.contains(field));
        }
    }

    #[test]
    fn quoted_commas_escapes_and_case_are_preserved() {
        let params = parse_digest_header(r#"dIgEsT REALM = "a,b", NONCE="a\"b", qop="auth-int, auth", opaque="x,y\\z", algorithm=md5"#).unwrap();
        assert_eq!(params.realm, "a,b");
        assert_eq!(params.nonce, "a\"b");
        assert_eq!(params.qop.as_deref(), Some("auth"));
        let header = build_digest_auth("a\"b\\c", "password", &params, "/", "GET").unwrap();
        assert!(header.contains(r#"username="a\"b\\c""#));
        assert!(header.contains(r#"opaque="x,y\\z""#));
    }

    #[test]
    fn malformed_ambiguous_and_unsupported_challenges_are_rejected() {
        for tail in [
            r#"realm="a",nonce="b",algorithm=SHA-256"#,
            r#"realm="a",nonce="b",algorithm=MD5-sess"#,
            r#"realm="a",nonce="b",qop="auth-int""#,
            r#"realm="a",nonce="b",nonce="c""#,
            r#"realm="a",nonce="b"#,
            r#"realm="a",nonce="""#,
            r#"realm="a",nonce="b" garbage"#,
            r#"realm="a",nonce="b",userhash=true"#,
        ] {
            assert!(
                parse_digest_header(&format!("Digest {tail}")).is_none(),
                "{tail}"
            );
        }
        assert!(parse_digest_header("Digest realm=\"a\", nonce=\"b\r\nx\"").is_none());
        let mut params = parse_digest_header(r#"Digest realm="a",nonce="b""#).unwrap();
        assert!(build_digest_auth("bad\r\nuser", "", &params, "/", "GET").is_err());
        params.qop = Some("auth-int".into());
        assert!(build_digest_auth("user", "", &params, "/", "GET").is_err());
    }
}
