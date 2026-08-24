# Cargo fixtures

These projects are inputs for scanner validation. They are deliberately dependency-free so tests
can inspect them or run Cargo without network access.

Most of them are not exercised yet, and the table says so per fixture. The bootstrapper derives
nothing from a Cargo package — it reads `[package.metadata.sonar]` and `[workspace.metadata.sonar]`
and contributes only `sonar.projectBaseDir`. Working out what a Cargo project looks like is the
scanner engine's job, so the assertions these fixtures exist for land once the corresponding engine
slice does.

| Fixture | Invocation directory | Asserted today | Behaviour it exists for |
| --- | --- | --- | --- |
| `single-crate` | `single-crate` | The manifest table is read, and the base directory is the fixture root. | Project identity derived from `[package]` — SCANENGINE-578. |
| `virtual-workspace` | `virtual-workspace` | `[workspace.metadata.sonar]` is read on a manifest that has no `[package]`. | One module per crate, no file claimed twice — SCANENGINE-579. |
| `virtual-workspace` | `virtual-workspace/crates/member` | **The base-directory contract**: a member invocation analyses the member and does not walk up to the workspace root. The two manifests declare different keys, so the assertion can fail. | One module per crate — SCANENGINE-579. |
| `workspace-inherited-fields` | `workspace-inherited-fields/crates/member` | Nothing beyond the manifest read. Inheritance is **not** exercised: the member's `version.workspace = true` is never resolved, because nothing reads `package.version`. | Members inherit `[workspace.package]` version and links — SCANENGINE-581. |
| `relocated-output-dir` | `relocated-output-dir` | Nothing. The fixture **does not relocate anything** — it sets `build-output/**` as an exclusion by hand. A real relocation needs `[build] target-dir` in `.cargo/config.toml` or `CARGO_TARGET_DIR`. | A relocated output directory is never indexed — SCANENGINE-582. |

End-to-end consumption of these fixtures against a real server and analyzer is SCANCARGO-4 and
SCANCARGO-6.

Note for whoever picks up SCANCARGO-6: explicit configuration always beats derived configuration,
so a fixture carrying `[package.metadata.sonar]` masks auto-configuration and cannot be used to test
it. Unconfigured variants will be needed. They are not added here because this ticket's
base-directory assertion depends on those tables being present.
