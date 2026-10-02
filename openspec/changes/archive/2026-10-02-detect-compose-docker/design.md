# Design

## Context

- `src/init_detect/compose.rs` maps compose images to service modules and calls `Draft::add_dep`.
- `service_manager` per dependency is accepted only on built-in services (`config.rs` `service_backend`).
- Every service module implements `Module::docker_spec`. A module that returns `None` cannot run as a container.

## Goals / Non-Goals

**Goals:**
- A compose-based project gets a `devy.yml` whose services run the same way compose runs them.
- Never emit a config that `devy up` would reject.

**Non-Goals:**
- Copying compose image names into `image:`. devy's default images are the official ones, and a mirror or a custom image is a deliberate choice better left to the user.
- Setting the top-level `service_manager`. Per-dependency keeps non-compose services unaffected and makes the origin of each choice visible.
- Translating compose ports, volumes or environment.

## Decisions

### D1. Per-dependency `service_manager: docker`
`DraftDep` gains `docker: bool`. The renderer writes a configured entry (`- redis:` with `service_manager: docker`, plus `version` when present) whenever the flag is set.

### D2. Native-tooling evidence suppresses docker
The detector checks for any of these in the project directory:
- `flake.nix`, `shell.nix`, `default.nix`, `devbox.json`, `Brewfile`
- an `.envrc` line that starts with `use nix` or `use flake`

`.envrc` is only searched for those directives and nothing from it is written or sent. When evidence is found, compose services are emitted without `service_manager` and one `# TODO:` says they could run as containers with `service_manager: docker`.

### D3. Earlier detections win
If a service was already added by another detector, such as `.tool-versions` listing `postgres`, the compose detector leaves it as it is: the project manages that service natively. This matches `Draft::add_dep`'s first-wins rule.

### D4. Only modules with a docker spec
The flag is set only when `docker_spec` returns `Some` for the dependency, so a future service module without container support degrades to a plain dependency rather than a config that fails at `devy up`.

## Risks / Trade-offs

- **[Docker not installed]** → `devy up` fails with the existing "docker is not available — … or set service_manager: package" message, which names the fix. The header already asks the user to review the file.
- **[Evidence heuristics miss a setup]** → The output is a draft with a review header, and `service_manager: package` is a one-line change.
