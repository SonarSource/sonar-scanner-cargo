# Cargo fixtures

These projects are inputs for scanner validation. They are deliberately dependency-free so tests
can inspect them or run Cargo without network access.

| Fixture | Invocation directory | Intended assertions |
| --- | --- | --- |
| `single-crate` | `single-crate` | Package-derived project key and name, fixture root as base directory, `target/**` exclusion. |
| `virtual-workspace` | `virtual-workspace` | A virtual workspace has no root package; its target directory is excluded. |
| `virtual-workspace` | `virtual-workspace/crates/member` | A member invocation uses the member as its base directory and derives its key and name from that package. |
| `workspace-inherited-fields` | `workspace-inherited-fields` | Cargo resolves member fields inherited from `workspace.package` before deriving project identity. |
| `relocated-output-dir` | `relocated-output-dir` | A package explicitly configures `build-output/**` as its scanner exclusion. |
