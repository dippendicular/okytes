//! Batch links: one pattern such as `https://host/file[001-100].zip` names a
//! numbered or lettered series. Pure text expansion, bounded by the caller's
//! limit; each resulting link is validated like any other.

/// Values of one `[from-to]` or `[from-to:step]` range, at most `limit + 1`
/// so the caller can tell "too many" without generating them all. `None`
/// means the bracket is not a range (for example an IPv6 host).
fn range(spec: &str, limit: usize) -> Option<Vec<String>> {
    let (bounds, step) = match spec.split_once(':') {
        Some((bounds, step)) => (bounds, step.parse::<usize>().ok().filter(|n| *n > 0)?),
        None => (spec, 1),
    };
    let (from, to) = bounds.split_once('-')?;
    let digits = |s: &str| !s.is_empty() && s.len() <= 18 && s.bytes().all(|b| b.is_ascii_digit());
    if digits(from) && digits(to) {
        let (first, last) = (from.parse::<u64>().ok()?, to.parse::<u64>().ok()?);
        // A leading zero asks for fixed-width numbers: [001-100].
        let width = if from.len() > 1 && from.starts_with('0') {
            from.len()
        } else {
            0
        };
        return (first <= last).then(|| {
            (first..=last)
                .step_by(step)
                .take(limit + 1)
                .map(|n| format!("{n:0width$}"))
                .collect()
        });
    }
    let letter = |s: &str| match s.as_bytes() {
        [b] if b.is_ascii_alphabetic() => Some(*b),
        _ => None,
    };
    let (first, last) = (letter(from)?, letter(to)?);
    (first <= last && first.is_ascii_lowercase() == last.is_ascii_lowercase()).then(|| {
        (first..=last)
            .step_by(step)
            .take(limit + 1)
            .map(|b| char::from(b).to_string())
            .collect()
    })
}

/// Expand every range in `link`. A link without a range expands to itself.
/// Several ranges multiply; more than `limit` results is an error.
pub fn expand(link: &str, limit: usize) -> Result<Vec<String>, String> {
    let mut links = vec![String::new()];
    let mut rest = link;
    while let Some(open) = rest.find('[') {
        let Some(close) = rest[open..].find(']').map(|at| open + at) else {
            break;
        };
        match range(&rest[open + 1..close], limit) {
            Some(values) => {
                if links.len().saturating_mul(values.len()) > limit {
                    return Err(format!(
                        "a batch pattern may name at most {limit} links"
                    ));
                }
                let (before, values) = (&rest[..open], &values);
                links = links
                    .iter()
                    .flat_map(|prefix| {
                        values
                            .iter()
                            .map(move |value| format!("{prefix}{before}{value}"))
                    })
                    .collect();
                rest = &rest[close + 1..];
            }
            None => {
                for link in &mut links {
                    link.push_str(&rest[..=open]);
                }
                rest = &rest[open + 1..];
            }
        }
    }
    for link in &mut links {
        link.push_str(rest);
    }
    Ok(links)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ranges_expand_with_padding_steps_letters_and_limits() {
        assert_eq!(
            expand("https://h/f[08-10].zip", 100).unwrap(),
            ["https://h/f08.zip", "https://h/f09.zip", "https://h/f10.zip"]
        );
        assert_eq!(
            expand("https://h/[1-5:2]/[a-b]", 100).unwrap(),
            [
                "https://h/1/a",
                "https://h/1/b",
                "https://h/3/a",
                "https://h/3/b",
                "https://h/5/a",
                "https://h/5/b"
            ]
        );
        // Not ranges: an IPv6 host, a reversed range, plain brackets.
        for link in ["http://[2001:db8::1]/a.bin", "https://h/[9-1]", "https://h/a[]b[x"] {
            assert_eq!(expand(link, 100).unwrap(), [link]);
        }
        assert_eq!(expand("https://h/[1-100]", 100).unwrap().len(), 100);
        assert!(expand("https://h/[1-101]", 100).is_err());
        assert!(expand("https://h/[1-20][1-20]", 100).is_err());
        assert!(expand("https://h/[0-99999999999999999]", 100).is_err());
    }
}
