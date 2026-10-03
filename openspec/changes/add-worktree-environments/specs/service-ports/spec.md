# Spec Delta

## MODIFIED Requirements

### Requirement: Port precedence during up
For every dependency whose module declares a port key, devy SHALL resolve the port in this order:
1. An explicit value in `devy.yml`.
2. When the active backend can apply a port to that service, the recorded port. In a linked git worktree, the recorded port is the port for the dependency's canonical name in `.devy/worktree.yml`. Otherwise, it is the `assigned_port` recorded for the dependency's canonical name in the existing `devy.lock`. In a linked worktree, devy MUST NOT use `devy.lock`'s `assigned_port`.
3. When the active backend can apply a port to that service, a free port chosen by the operating system (`devy up` only).
4. Otherwise, the module's default port.

#### Scenario: Explicit port wins
- **WHEN** `devy.yml` declares `redis` with `port: 6380` and `devy.lock` records `assigned_port: 51000`
- **THEN** devy uses port 6380

#### Scenario: Locked port reused
- **WHEN** the backend is nix, `devy.yml` declares `redis` without a port, and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy uses port 51000

#### Scenario: Random port assigned
- **WHEN** the backend is nix, `devy.yml` declares `redis` without a port, and there is no lock entry for it
- **THEN** `devy up` binds `127.0.0.1:0`, uses the port the OS assigns, and fails with `Failed to find available port for redis` if that is impossible

#### Scenario: Backend cannot apply the port
- **WHEN** the backend is brew, `devy.yml` declares `redis` without a port, and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy uses redis's default port 6379, and `REDIS_PORT` and `REDIS_URL` reference 6379

#### Scenario: Worktree ignores the lock's port
- **WHEN** the backend is nix, `devy up` runs in a linked worktree, `devy.lock` records `assigned_port: 51000` for `redis`, and `.devy/worktree.yml` has no entry for `redis`
- **THEN** devy assigns a new free port other than 51000 to `redis` and exports it as `REDIS_PORT`

#### Scenario: Worktree reuses its own port
- **WHEN** the backend is nix, the project is a linked worktree, and `.devy/worktree.yml` records `redis: 52000`
- **THEN** devy uses port 52000

### Requirement: Ports survive update
Port resolution SHALL read the existing recorded ports, either `devy.lock` or, in a linked worktree, `.devy/worktree.yml`, even when `devy up --update` ignores the lock for version pinning.

#### Scenario: Update keeps ports
- **WHEN** the user runs `devy up --update` and `devy.lock` records `assigned_port: 51000` for `redis`
- **THEN** devy still uses port 51000 for `redis`

#### Scenario: Update keeps worktree ports
- **WHEN** the user runs `devy up --update` in a linked worktree whose `.devy/worktree.yml` records `redis: 52000`
- **THEN** devy still uses port 52000 for `redis`

### Requirement: Port persistence in the lock file
Outside a linked git worktree, `devy up` SHALL record the resolved port as `assigned_port` in the dependency's `devy.lock` entry for every module that has a port key and whose port the active backend can apply, including ports set explicitly in `devy.yml`. For services whose port the backend cannot apply, `devy up` MUST NOT record an `assigned_port`.

In a linked worktree, `devy up` SHALL record each such resolved port in `.devy/worktree.yml` instead. It SHALL write each dependency's `assigned_port` in `devy.lock` exactly as the existing `devy.lock` had it, and leave it absent where it was absent, so a worktree never changes the committed ports.

#### Scenario: Port recorded
- **WHEN** `devy up` assigns port 51000 to `redis` under the nix backend
- **THEN** `devy.lock` contains `assigned_port: 51000` under `redis`

#### Scenario: Port not recorded where it cannot be applied
- **WHEN** `devy up` runs with the brew backend and `devy.yml` declares `redis` with `port: 6380`
- **THEN** the `redis` entry in `devy.lock` has no `assigned_port`

#### Scenario: Worktree leaves lock ports alone
- **WHEN** `devy.lock` records `assigned_port: 51000` for `redis`, and `devy up` in a linked worktree assigns 52000
- **THEN** `.devy/worktree.yml` records `redis: 52000`, `devy.lock` still records `assigned_port: 51000`, and if nothing else changed, `devy.lock` is not rewritten

#### Scenario: New service in a worktree
- **WHEN** a worktree's `devy.yml` adds `memcached`, which has no entry in `devy.lock`, and the user runs `devy up` there with the nix backend
- **THEN** the new `memcached` entry in `devy.lock` has no `assigned_port`, and its port is recorded only in `.devy/worktree.yml`

### Requirement: Consistent port resolution across commands
`devy start`, `devy restart`, `devy check`, and `devy status` SHALL resolve service ports with the same precedence as `devy up`, using `devy.lock` or, in a linked worktree, `.devy/worktree.yml`. They MUST NOT assign new random ports, write `devy.lock`, or write `.devy/worktree.yml`. When a port would be randomly assigned by `devy up` but is not yet recorded, `devy check` SHALL treat it as not yet assigned and exclude it from conflict detection. `devy start` and `devy restart` SHALL instead fail with `'<name>' has no port in devy.lock yet — run `devy up` first`, or, in a linked worktree, `'<name>' has no port in this worktree yet — run `devy up` first`.

#### Scenario: Start uses the locked port
- **WHEN** the backend is nix, `devy.lock` records `assigned_port: 51000` for `redis`, and the user runs `devy start redis`
- **THEN** redis is launched on port 51000 and the health check probes port 51000

#### Scenario: Start before up
- **WHEN** the backend is nix, `devy.lock` has no entry for `redis`, and the user runs `devy start redis`
- **THEN** devy fails, telling the user to run `devy up` first, and starts nothing

#### Scenario: Start in a worktree before up
- **WHEN** the backend is nix, the project is a linked worktree, `devy.lock` records `assigned_port: 51000` for `redis`, `.devy/worktree.yml` has no entry for `redis`, and the user runs `devy start redis`
- **THEN** devy fails with `'redis' has no port in this worktree yet — run `devy up` first` and does not use port 51000

#### Scenario: Check agrees with up on unassigned ports
- **WHEN** the backend is nix, `devy.yml` declares `mysql` and `mariadb` with no ports, and `devy.lock` has no entries for them
- **THEN** `devy check` does not report a port conflict between them
