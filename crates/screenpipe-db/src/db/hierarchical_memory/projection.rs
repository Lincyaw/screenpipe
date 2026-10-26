// screenpipe — AI that knows everything you've seen, said, or heard
// https://screenpipe.com

use super::*;

/// Project/document identity takes precedence over app identity. This links a
/// repository opened in browser, editor and terminal into the same task. When
/// deterministic identity is weak, the honest label remains user-correctable.
pub(super) fn topic(title: &str, url: Option<&str>, path: Option<&str>) -> String {
    if let Some(url) = url {
        if let Some(rest) = url.split("github.com/").nth(1) {
            if let Some(repo) = rest.split('/').nth(1).filter(|s| !s.is_empty()) {
                return repo.split(['?', '#']).next().unwrap_or(repo).to_lowercase();
            }
        }
    }
    if let Some(path) = path {
        for root in ["/workspace/", "/projects/", "/repos/", "/work/"] {
            if let Some(project) = path
                .split(root)
                .nth(1)
                .and_then(|p| p.split('/').next())
                .filter(|p| !p.is_empty())
            {
                return project.to_lowercase();
            }
        }
    }
    // Browsers and editors can append profiles/workspaces after their app
    // chrome ("document - Google Chrome - profile"). Cut at that boundary so
    // the same document remains one purpose across applications.
    let boundary = [
        " - Google Chrome",
        " — Google Chrome",
        " - Chromium",
        " - Brave",
        " - Microsoft Edge",
        " — Mozilla Firefox",
        " - Mozilla Firefox",
        " - Visual Studio Code",
        " — Visual Studio Code",
    ]
    .into_iter()
    .filter_map(|suffix| title.find(suffix))
    .min()
    .unwrap_or(title.len());
    let value = title[..boundary].trim();
    // Browser document titles often carry the same repository name as editor
    // window titles. Strip activity verbs rather than introducing an App layer.
    let value = value
        .split(" · ")
        .next()
        .unwrap_or(value)
        .split(" | ")
        .next()
        .unwrap_or(value)
        .trim();
    bounded(
        if value.is_empty() {
            "Unclassified activity"
        } else {
            value
        },
        180,
    )
    .to_lowercase()
}
