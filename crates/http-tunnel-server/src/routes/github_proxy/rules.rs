use super::matcher::GithubUrlMatch;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessDecision {
    Proxy,
    PassBy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AccessDenied {
    WhiteList,
    BlackList,
}

pub(crate) fn evaluate(
    matched: &GithubUrlMatch,
    white_list: &[String],
    black_list: &[String],
    pass_list: &[String],
) -> Result<AccessDecision, AccessDenied> {
    let white_rules = parse_rules(white_list);
    if !white_rules.is_empty() && !white_rules.iter().any(|rule| rule.matches(matched)) {
        return Err(AccessDenied::WhiteList);
    }

    let black_rules = parse_rules(black_list);
    if black_rules.iter().any(|rule| rule.matches(matched)) {
        return Err(AccessDenied::BlackList);
    }

    let pass_rules = parse_rules(pass_list);
    if pass_rules.iter().any(|rule| rule.matches(matched)) {
        return Ok(AccessDecision::PassBy);
    }
    Ok(AccessDecision::Proxy)
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Rule {
    owner: String,
    repo: Option<String>,
}

impl Rule {
    fn matches(&self, matched: &GithubUrlMatch) -> bool {
        if self.owner != "*" && self.owner != matched.owner {
            return false;
        }
        match (&self.repo, &matched.repo) {
            (None, _) => self.owner != "*",
            (Some(expected), Some(repo)) => expected == repo,
            (Some(_), None) => false,
        }
    }
}

fn parse_rules(values: &[String]) -> Vec<Rule> {
    values
        .iter()
        .filter_map(|value| parse_rule(value))
        .collect()
}

fn parse_rule(value: &str) -> Option<Rule> {
    let normalized = value.replace(' ', "");
    let mut parts = normalized.split('/').filter(|part| !part.is_empty());
    let owner = parts.next()?.to_string();
    let repo = parts.next().map(ToString::to_string);
    if parts.next().is_some() || owner == "*" && repo.is_none() {
        return None;
    }
    Some(Rule { owner, repo })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::routes::github_proxy::matcher::{GithubUrlKind, GithubUrlMatch};

    fn repo(owner: &str, repo: &str) -> GithubUrlMatch {
        GithubUrlMatch {
            kind: GithubUrlKind::ReleaseOrArchive,
            owner: owner.to_string(),
            repo: Some(repo.to_string()),
        }
    }

    fn gist(owner: &str) -> GithubUrlMatch {
        GithubUrlMatch {
            kind: GithubUrlKind::Gist,
            owner: owner.to_string(),
            repo: None,
        }
    }

    #[test]
    fn white_list_must_match_when_configured() {
        let decision = evaluate(&repo("owner", "repo"), &["owner/repo".into()], &[], &[]);
        assert_eq!(decision, Ok(AccessDecision::Proxy));

        let decision = evaluate(&repo("other", "repo"), &["owner/repo".into()], &[], &[]);
        assert_eq!(decision, Err(AccessDenied::WhiteList));
    }

    #[test]
    fn black_list_blocks_after_white_list() {
        let decision = evaluate(
            &repo("owner", "repo"),
            &["owner".into()],
            &["owner/repo".into()],
            &[],
        );
        assert_eq!(decision, Err(AccessDenied::BlackList));
    }

    #[test]
    fn pass_list_marks_proxy_bypass() {
        let decision = evaluate(&repo("owner", "repo"), &[], &[], &["*/repo".into()]);
        assert_eq!(decision, Ok(AccessDecision::PassBy));
    }

    #[test]
    fn owner_only_rule_matches_gists() {
        let decision = evaluate(&gist("owner"), &["owner".into()], &[], &[]);
        assert_eq!(decision, Ok(AccessDecision::Proxy));

        let decision = evaluate(&gist("owner"), &["*/repo".into()], &[], &[]);
        assert_eq!(decision, Err(AccessDenied::WhiteList));
    }
}
