//! The container Freightdock itself runs in, found at start on the local Docker. A console there is root beside
//! Freightdock's data folder and, as the image is run, the Docker socket: whoever has it has every environment, so it is
//! an Administrator's alone, whatever their role on the local environment ("Access" 20).

use crate::error::{ApiError, ApiResult};
use crate::state::App;

/// The container ids this process's mounts and control groups name: Docker mounts a container's `hostname`,
/// `hosts` and `resolv.conf` from `.../containers/<id>/`, and a cgroup v1 path ends in the id.
pub fn candidates(mountinfo: &str, cgroup: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    for text in [mountinfo, cgroup] {
        for (i, _) in text.match_indices("containers/").chain(text.match_indices("docker-")).chain(text.match_indices("docker/")) {
            let rest = &text[i..];
            let rest = &rest[rest.find(['/', '-']).map_or(0, |k| k + 1)..];
            let id: String = rest.chars().take_while(|c| c.is_ascii_hexdigit()).collect();
            if id.len() == 64 && !out.contains(&id) {
                out.push(id);
            }
        }
    }
    out
}

/// The id of the container Freightdock runs in on the local Docker, if it runs in one there: the first candidate the
/// local Docker knows.
pub async fn detect(docker: &super::Docker) -> Option<String> {
    let read = |p: &str| std::fs::read_to_string(p).unwrap_or_default();
    for id in candidates(&read("/proc/self/mountinfo"), &read("/proc/self/cgroup")) {
        if let Ok(c) = docker.client().inspect_container(&id, None::<bollard::query_parameters::InspectContainerOptions>).await
            && let Some(found) = c.id
        {
            return Some(found);
        }
    }
    None
}

/// Whether container `id` (an id or a name) on environment `env` is the one Freightdock runs in: only the local
/// environment can hold it.
pub async fn is_own(app: &App, env: i64, id: &str) -> ApiResult<bool> {
    let Some(own) = app.own_container.get().cloned() else { return Ok(false) };
    let kind: Option<String> = app
        .db
        .call(move |c| {
            use crate::db::rusqlite::OptionalExtension;
            Ok(c.query_row("SELECT kind FROM environments WHERE id = ?1", [env], |r| r.get(0)).optional()?)
        })
        .await?;
    if kind.as_deref() != Some("local") {
        return Ok(false);
    }
    if id == own {
        return Ok(true);
    }
    match app.docker.client().inspect_container(id, None::<bollard::query_parameters::InspectContainerOptions>).await {
        Ok(c) => Ok(c.id.as_deref() == Some(own.as_str())),
        Err(bollard::errors::Error::DockerResponseServerError { status_code: 404, .. }) => Ok(false),
        Err(e) => Err(super::api_error(e)),
    }
}

/// The refusal.
pub fn refused() -> ApiError {
    ApiError::new(axum::http::StatusCode::FORBIDDEN, "Only an administrator may open a console in the container Freightdock runs in.")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_container_is_found_in_the_mounts_or_the_control_groups() {
        let id = "4f1c0a7d9e2b".repeat(5) + "abcd";
        assert_eq!(id.len(), 64);
        let mountinfo = format!(
            "612 590 0:53 / / rw,relatime master:1 - overlay overlay rw\n\
             640 612 259:2 /var/lib/docker/containers/{id}/resolv.conf /etc/resolv.conf rw - ext4 /dev/nvme0n1p2 rw\n\
             641 612 259:2 /var/lib/docker/containers/{id}/hostname /etc/hostname rw - ext4 /dev/nvme0n1p2 rw\n"
        );
        assert_eq!(candidates(&mountinfo, "0::/\n"), std::slice::from_ref(&id));
        assert_eq!(candidates("", &format!("12:memory:/docker/{id}\n")), std::slice::from_ref(&id));
        assert_eq!(candidates("", &format!("0::/system.slice/docker-{id}.scope\n")), std::slice::from_ref(&id));
        assert!(candidates("612 590 0:53 / / rw - overlay overlay rw\n", "0::/user.slice\n").is_empty(), "not in a container");
        assert!(candidates("/var/lib/docker/containers/abc/hostname", "").is_empty(), "only a whole id");
    }
}
