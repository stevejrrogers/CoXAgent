
            /// Returns the `rustc` SemVer version and additional metadata
            /// like the git short hash and build date.
            pub fn version_meta() -> VersionMeta {
                VersionMeta {
                    semver: Version {
                        major: 1,
                        minor: 97,
                        patch: 1,
                        pre: Prerelease::new("").unwrap(),
                        build: BuildMetadata::new("").unwrap(),
                    },
                    host: "aarch64-apple-darwin".to_owned(),
                    short_version_string: "rustc 1.97.1 (8bab26f4f 2026-07-14)".to_owned(),
                    commit_hash: Some("8bab26f4f68e0e26f0bb7960be334d5b520ea452".to_owned()),
                    commit_date: Some("2026-07-14".to_owned()),
                    build_date: None,
                    channel: Channel::Stable,
                    llvm_version: Some(LlvmVersion{ major: 22, minor: 1 }),
                }
            }
            