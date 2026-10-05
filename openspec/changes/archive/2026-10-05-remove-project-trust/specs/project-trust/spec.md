# project-trust delta

## REMOVED Requirements

### Requirement: Trust records
**Reason**: The project-trust capability is retired. Running `devy up` in a repository is no different from running a shell script in it, so users vet repositories themselves, as they do any other code they run.
**Migration**: None. devy no longer reads or writes `$XDG_STATE_HOME/devy/trust/` (`%LOCALAPPDATA%\devy\trust\`); existing records are ignored and can be deleted.

### Requirement: Allow command
**Reason**: The project-trust capability is retired. Running `devy up` in a repository is no different from running a shell script in it, so users vet repositories themselves, as they do any other code they run.
**Migration**: Remove `devy allow` from CI jobs and scripts; `devy up` runs without it. `devy allow` is no longer a subcommand.

### Requirement: Trust summary
**Reason**: The project-trust capability is retired. Running `devy up` in a repository is no different from running a shell script in it, so users vet repositories themselves, as they do any other code they run.
**Migration**: The listing of what a configuration runs, with its secret masking, remains where AI `init` and AI `doctor` review executable entries, as the project-config requirement "Executable entry listing".

### Requirement: Trust gate on commands that run project code
**Reason**: The project-trust capability is retired. Running `devy up` in a repository is no different from running a shell script in it, so users vet repositories themselves, as they do any other code they run.
**Migration**: None. `devy up`, `devy down`, `devy start`, `devy restart` and `devy exec` run without a trust check; review `devy.yml` before running devy in a repository you did not write. The managed-path checks (filesystem-safety) and the shadowenv checks (shell-environment, shell-integration) still run.

### Requirement: devy keeps trust current for its own writes
**Reason**: The project-trust capability is retired. Running `devy up` in a repository is no different from running a shell script in it, so users vet repositories themselves, as they do any other code they run.
**Migration**: None. There is no trust record to keep current; `devy doctor --yes` still refuses fixes that change executable fields.
