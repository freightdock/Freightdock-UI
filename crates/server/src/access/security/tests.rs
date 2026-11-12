use super::*;

fn locked() -> Rules {
    Rules::locked_down()
}

fn settings(b: &[Breach]) -> Vec<Setting> {
    b.iter().map(|b| b.setting).collect()
}

#[test]
fn by_default_everything_is_allowed_as_before_there_were_settings() {
    let r = Rules::default();
    let all = of_container(&json!({ "HostConfig": {
        "Privileged": true, "Binds": ["/:/host"], "PidMode": "host", "IpcMode": "host", "NetworkMode": "host", "UTSMode": "host",
        "Devices": [{ "PathOnHost": "/dev/kvm" }], "CapAdd": ["SYS_ADMIN"], "SecurityOpt": ["seccomp=unconfined"], "Sysctls": { "net.ipv4.ip_forward": "1" },
    } }));
    assert!(judge(&r, &all).is_empty());
    assert!(!r.judges_specs());
    assert!(!Rules { block_consoles: true, ..Default::default() }.judges_specs(), "consoles are not about what a container is made with");
    assert!(locked().judges_specs());
    assert!(!locked().block_consoles, "Locked down leaves consoles open: they are an Operator's work");
}

#[test]
fn locked_down_refuses_each_thing_that_reaches_the_host_on_its_own_field() {
    let r = locked();
    let one = |hc: Value| judge(&r, &of_container(&json!({ "HostConfig": hc })));
    assert_eq!(settings(&one(json!({ "Privileged": true }))), [Setting::Privileged]);
    assert_eq!(settings(&one(json!({ "PidMode": "host" }))), [Setting::HostPid]);
    assert_eq!(settings(&one(json!({ "IpcMode": "host" }))), [Setting::HostIpc]);
    assert_eq!(settings(&one(json!({ "NetworkMode": "host" }))), [Setting::HostNetwork]);
    assert_eq!(settings(&one(json!({ "UTSMode": "host" }))), [Setting::HostUts]);
    assert_eq!(settings(&one(json!({ "Devices": [{ "PathOnHost": "/dev/kvm", "PathInContainer": "/dev/kvm" }] }))), [Setting::Devices]);
    assert_eq!(settings(&one(json!({ "DeviceRequests": [{ "Driver": "nvidia", "Count": -1 }] }))), [Setting::Devices]);
    assert_eq!(settings(&one(json!({ "CapAdd": ["NET_ADMIN", "SYS_ADMIN"] }))), [Setting::CapAdd, Setting::CapAdd]);
    assert_eq!(settings(&one(json!({ "SecurityOpt": ["apparmor=unconfined"] }))), [Setting::SecurityOpt]);
    assert_eq!(settings(&one(json!({ "Sysctls": { "net.core.somaxconn": "1024" } }))), [Setting::Sysctls]);
    assert!(one(json!({ "NetworkMode": "bridge", "PidMode": "", "Privileged": false, "CapAdd": [] })).is_empty(), "what is not asked for is not refused");
    assert_eq!(Setting::Privileged.field(), "privileged");
    assert_eq!(Setting::BindMount.field(), "binds");
    assert_eq!(Setting::HostNetwork.field(), "network_mode");
    let b = one(json!({ "Privileged": true }));
    let e = refusal(&b, None);
    assert_eq!((e.status, e.field), (StatusCode::FORBIDDEN, Some("privileged")));
    assert_eq!(e.message, "Privileged mode is turned off for non-administrators on this host.");
}

#[test]
fn a_host_path_is_refused_but_a_named_volume_and_the_allow_list_are_not() {
    let r = Rules { bind_allow: vec!["/srv/apps".into(), "/mnt/media/".into()], ..locked() };
    let binds = |b: Value| judge(&r, &of_container(&json!({ "HostConfig": { "Binds": b } })));
    assert!(binds(json!(["data:/data", "cache:/var/cache:ro"])).is_empty(), "named volumes are Docker's, not the host's");
    assert!(binds(json!(["/srv/apps/shop:/app", "/srv/apps:/all:ro", "/mnt/media:/media"])).is_empty(), "under a prefix, or the prefix itself");
    let b = binds(json!(["/srv/appsecret:/x", "/etc:/etc:ro", "/srv/apps/../../etc:/y"]));
    assert_eq!(b.iter().map(|b| b.what.as_str()).collect::<Vec<_>>(), ["/srv/appsecret", "/etc", "/srv/apps/../../etc"], "a prefix is whole steps, and .. climbs out of any");
    assert_eq!(b[0].setting, Setting::BindMount);
    let mounts = judge(&r, &of_container(&json!({ "HostConfig": { "Mounts": [
        { "Type": "bind", "Source": "/var/run/docker.sock", "Target": "/var/run/docker.sock" },
        { "Type": "volume", "Source": "data", "Target": "/d" },
        { "Type": "volume", "Source": "sneaky", "Target": "/s", "VolumeOptions": { "DriverConfig": { "Name": "local", "Options": { "type": "none", "o": "bind", "device": "/root" } } } },
    ] } })));
    assert_eq!(mounts.iter().map(|b| b.what.as_str()).collect::<Vec<_>>(), ["/var/run/docker.sock", "/root"], "a volume that is a host path underneath is one");
    assert_eq!(settings(&binds(json!(["/dev/sda:/disk"]))), [Setting::Devices], "a bind of a device is a device");
    let only_binds = Rules { block_bind_mounts: true, ..Default::default() };
    assert_eq!(settings(&judge(&only_binds, &of_container(&json!({ "HostConfig": { "Binds": ["/dev/sda:/disk"] } })))), [Setting::BindMount], "and still a host path when devices are allowed");
    assert!(judge(&Rules { bind_allow: vec!["/".into()], ..locked() }, &of_container(&json!({ "HostConfig": { "Binds": ["/etc:/etc"] } }))).is_empty(), "/ allows every path");
}

#[test]
fn host_paths_are_read_as_whole_steps() {
    assert_eq!(normal("/srv//apps/./x/"), Some("/srv/apps/x".into()));
    assert_eq!(normal("/"), Some("/".into()));
    assert_eq!(normal("srv"), None);
    assert_eq!(normal("/a/../b"), None);
    assert!(allowed_path(&["/srv".into()], "/srv/x"));
    assert!(!allowed_path(&["/srv".into()], "/srvx"));
    assert!(!allowed_path(&[], "/srv"));
    assert!(Rules { bind_allow: vec!["relative".into()], ..Default::default() }.checked().is_err());
    assert_eq!(Rules { bind_allow: vec![" /srv/ ".into(), "".into()], ..Default::default() }.checked().unwrap().bind_allow, ["/srv"]);
}

#[test]
fn an_edit_is_held_only_to_what_it_adds() {
    let r = locked();
    let old = judge(&r, &of_container(&json!({ "HostConfig": { "Privileged": true, "Binds": ["/srv:/srv"] } })));
    let new = judge(&r, &of_container(&json!({ "HostConfig": { "Privileged": true, "Binds": ["/srv:/srv", "/etc:/etc"] } })));
    let a = added(new, &old);
    assert_eq!(a.len(), 1, "what an administrator set stays; the new host path does not: {a:?}");
    assert_eq!(a[0].what, "/etc");
}

#[test]
fn a_stack_is_judged_service_by_service_from_the_file_compose_resolves() {
    // As `docker compose config --format json` writes it.
    let config = json!({
        "name": "shop",
        "services": {
            "web": { "image": "nginx", "network_mode": "host", "volumes": [{ "type": "bind", "source": "/etc/nginx", "target": "/etc/nginx" }, { "type": "volume", "source": "data", "target": "/d" }] },
            "db": { "image": "postgres", "privileged": true, "pid": "host", "devices": [{ "source": "/dev/fuse", "target": "/dev/fuse", "permissions": "rwm" }], "cap_add": ["SYS_PTRACE"], "security_opt": ["label=disable"], "sysctls": { "net.core.somaxconn": "1024" } },
            "old": { "image": "x", "devices": ["/dev/ttyUSB0:/dev/ttyUSB0"], "ipc": "host", "uts": "host", "volumes": [{ "type": "volume", "source": "host", "target": "/h" }] },
            "gpu": { "image": "x", "deploy": { "resources": { "reservations": { "devices": [{ "capabilities": ["gpu"], "driver": "nvidia" }] } } } },
            "fine": { "image": "x", "volumes": [{ "type": "volume", "source": "data", "target": "/d" }] },
        },
        "volumes": { "data": { "name": "shop_data" }, "host": { "driver_opts": { "type": "none", "o": "bind", "device": "/home" } } },
    });
    let specs = of_compose(&config);
    let r = locked();
    let by = |svc: &str| -> Vec<Setting> { specs.iter().find(|s| s.place.as_deref() == Some(&format!("Service {svc}"))).map(|s| settings(&judge(&r, s))).unwrap() };
    assert_eq!(by("web"), [Setting::BindMount, Setting::HostNetwork]);
    assert_eq!(by("db"), [Setting::Privileged, Setting::HostPid, Setting::Devices, Setting::CapAdd, Setting::SecurityOpt, Setting::Sysctls]);
    assert_eq!(by("old"), [Setting::BindMount, Setting::HostIpc, Setting::HostUts, Setting::Devices], "the volume that is /home underneath, and a device written short");
    assert_eq!(by("gpu"), [Setting::Devices]);
    assert!(by("fine").is_empty());
    let web: Vec<Breach> = specs.iter().filter(|s| s.place.as_deref() == Some("Service web")).flat_map(|s| judge(&r, s)).collect();
    assert_eq!(refusal(&web, Some("compose")).message, "Service web: A bind mount of a host path (/etc/nginx) is turned off for non-administrators on this host. Service web: The host's network (network: host) is turned off for non-administrators on this host.");
    assert_eq!(refusal(&web, Some("compose")).field, Some("compose"));
}

#[test]
fn a_swarm_service_is_judged_from_its_spec() {
    let spec = json!({
        "Name": "agent",
        "TaskTemplate": {
            "ContainerSpec": {
                "Image": "x",
                "Mounts": [{ "Type": "bind", "Source": "/var/run/docker.sock", "Target": "/var/run/docker.sock" }, { "Type": "bind", "Source": "/dev/kvm", "Target": "/dev/kvm" }],
                "CapabilityAdd": ["NET_ADMIN"], "Sysctls": { "net.ipv4.ip_forward": "1" },
                "Privileges": { "Seccomp": { "Mode": "unconfined" }, "AppArmor": { "Mode": "default" } },
            },
            "Networks": [{ "Target": "host" }],
            "Resources": { "Reservations": { "GenericResources": [{ "DiscreteResourceSpec": { "Kind": "gpu", "Value": 1 } }] } },
        },
    });
    let b = judge(&locked(), &of_service(&spec));
    assert_eq!(settings(&b), [Setting::BindMount, Setting::Devices, Setting::HostNetwork, Setting::Devices, Setting::CapAdd, Setting::SecurityOpt, Setting::Sysctls]);
    assert_eq!(b[0].place.as_deref(), Some("Service agent"));
    assert!(judge(&locked(), &of_service(&json!({ "Name": "web", "TaskTemplate": { "ContainerSpec": { "Image": "nginx" } } }))).is_empty());
}

#[test]
fn a_volume_that_is_a_host_path_underneath_is_a_bind_mount() {
    let v = of_volume(&json!({ "Name": "root", "Driver": "local", "DriverOpts": { "type": "none", "o": "bind", "device": "/" } }));
    assert_eq!(settings(&judge(&locked(), &v)), [Setting::BindMount]);
    assert!(judge(&locked(), &of_volume(&json!({ "Name": "data" }))).is_empty());
    assert!(judge(&locked(), &of_volume(&json!({ "Name": "nfs", "DriverOpts": { "type": "nfs", "o": "addr=192.0.2.1", "device": ":/export" } }))).is_empty());
}

#[test]
fn only_an_administrator_and_an_environment_administrator_are_above_the_rules() {
    assert!(exempt(Level::Admin));
    assert!(exempt(Level::Role(Role::EnvAdmin)));
    for r in [Role::Operator, Role::Helpdesk, Role::Standard, Role::ReadOnly] {
        assert!(!exempt(Level::Role(r)), "{r:?}");
    }
}

#[tokio::test]
async fn an_environment_follows_its_group_until_it_has_its_own() {
    let db = Db::open_in_memory().unwrap();
    let f = db
        .call(|c| {
            c.execute_batch(
                "INSERT INTO environment_groups (id, name, created_at) VALUES (10, 'prod', 0);
                 INSERT INTO environments (id, name, kind, url, created_at, group_id) VALUES (1, 'local', 'local', '', 0, NULL), (2, 'web', 'agent', '', 0, 10);",
            )?;
            let none = (found(c, 1)?, found(c, 2)?);
            c.execute("INSERT INTO security_rules (group_id, rules, updated_at) VALUES (10, ?1, 0)", [serde_json::to_string(&Rules::locked_down()).unwrap()])?;
            let inherited = (found(c, 1)?, found(c, 2)?);
            c.execute("INSERT INTO security_rules (env_id, rules, updated_at) VALUES (2, ?1, 0)", [serde_json::to_string(&Rules { block_privileged: true, ..Default::default() }).unwrap()])?;
            let own = found(c, 2)?;
            c.execute("DELETE FROM environment_groups WHERE id = 10", [])?;
            let gone = c.query_row("SELECT COUNT(*) FROM security_rules WHERE group_id IS NOT NULL", [], |r| r.get::<_, i64>(0))?;
            Ok((none, inherited, own, gone))
        })
        .await
        .unwrap();
    let (none, inherited, own, gone) = f;
    assert_eq!((none.0.rules.clone(), none.1.rules.clone()), (Rules::default(), Rules::default()));
    assert_eq!(none.1.group.as_ref().map(|g| g.1.as_str()), Some("prod"));
    assert_eq!(inherited.0.rules, Rules::default(), "not in the group");
    assert_eq!(inherited.1.rules, Rules::locked_down(), "the group's");
    assert_eq!(own.rules, Rules { block_privileged: true, ..Default::default() }, "the environment's own wins, whole");
    assert_eq!(own.group.unwrap().2, Some(Rules::locked_down()));
    assert_eq!(gone, 0, "a group's rules go with it");
}
