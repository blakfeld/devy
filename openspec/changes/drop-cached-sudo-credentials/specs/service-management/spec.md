## MODIFIED Requirements

### Requirement: Apt (systemd) service backend
When the package manager is apt, devy SHALL control services with `/usr/bin/sudo -k <systemctl> start|stop -- <name>`, where `<systemctl>` is `/usr/bin/systemctl` (or `/bin/systemctl`). `-k` means each start or stop asks for the password unless sudoers grants `NOPASSWD`, and leaves no cached credential for hooks run afterwards. devy SHALL treat a service as running when `systemctl is-active <name>` reports `active`.

#### Scenario: Status via systemctl
- **WHEN** `systemctl is-active redis` prints `active`
- **THEN** devy reports `redis` as running

#### Scenario: Start via sudo without caching
- **WHEN** devy starts `redis-server` with the apt backend on a system with `/usr/bin/systemctl`
- **THEN** it runs `/usr/bin/sudo -k /usr/bin/systemctl start -- redis-server`

#### Scenario: Stop via sudo without caching
- **WHEN** devy stops `redis-server` with the apt backend on a system with `/usr/bin/systemctl`
- **THEN** it runs `/usr/bin/sudo -k /usr/bin/systemctl stop -- redis-server`
