#!/usr/bin/env bash
# End-to-end check of the `devy up` failure record and `devy doctor --no-ai`:
# break a project with a port conflict in devy.lock, confirm `devy up` records the
# failure and suggests doctor, diagnose it offline, then fix it and confirm a
# successful `devy up` removes the record.
#
# Usage: doctor-e2e.sh <path to devy binary>
set -euo pipefail

DEVY=$1
WORK=$(mktemp -d)
# A trust store of our own, so the run never touches the user's.
STATE=$(mktemp -d)
export XDG_STATE_HOME="$STATE"
trap 'rm -rf "$WORK" "$STATE"' EXIT
cd "$WORK"
git init -q

cat > devy.yml <<'EOF'
name: smoke-doctor
package_manager: nix
dependencies:
  - redis
  - postgresql
EOF

# Both services locked to the same port, so `devy up` fails resolving ports.
cat > devy.lock <<'EOF'
version: 1
dependencies:
  redis:
    resolved_version: null
    source: nix
    assigned_port: 16399
  postgresql:
    resolved_version: null
    source: nix
    assigned_port: 16399
EOF

# devy runs a project's code only once it is allowed.
"$DEVY" allow
if "$DEVY" up > up.out 2> up.err; then
  echo "devy up should have failed on the port conflict" >&2
  exit 1
fi
cat up.out up.err
grep -q "error: .*port conflict" up.err
grep -q "run devy doctor to diagnose this failure" up.err
grep -q '"step": "resolve ports"' .devy/last-up-failure.json

"$DEVY" doctor --no-ai > doctor.out 2> doctor.err
cat doctor.out doctor.err
grep -q "Last devy up failure" doctor.out
grep -q "step: resolve ports" doctor.out
grep -q "port conflict" doctor.err
grep -q "AI diagnosis unavailable — disabled with --no-ai" doctor.out

# Fix the project; the stale lock entries become orphans.
cat > devy.yml <<'EOF'
name: smoke-doctor
package_manager: nix
dependencies:
  - jq
EOF
# devy.yml changed, so the project must be allowed again.
"$DEVY" allow
"$DEVY" up
test ! -e .devy/last-up-failure.json
echo "doctor e2e passed"
