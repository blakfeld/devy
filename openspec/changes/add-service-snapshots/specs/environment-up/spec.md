# Spec Delta

## MODIFIED Requirements

### Requirement: Service start phase
After the lock is written, `devy up` SHALL start each service dependency in declaration order and then wait for it to become ready:
- If the service is already running, it SHALL print `○ <dep> service already running`.
- If starting fails, it SHALL fail with `Failed to start <dep> service`.
- If the health check does not pass in time, it SHALL print the warning `<dep> is not yet responding to health checks — verify manually: <cause>`, and SHALL NOT fail the run.

Once a service that declares `seed` is ready, `devy up` SHALL handle its seed before moving to the next service (see service-seeds):
- **No seed recorded:** apply the seed, printing `→ Seeding <dep>` and then `✓ <dep> seeded`.
- **Seed recorded with a matching fingerprint:** print `○ <dep> already seeded`.
- **Seed recorded with a different fingerprint:** print the seed-changed warning.
- **Service not ready:** skip seeding, warn `<dep> not ready — skipping seed; run devy seed <dep> once it is up`, and SHALL NOT fail the run.
- **Seeding fails:** fail with `Failed to seed <dep>`.

Because a start or seed failure aborts the run, any later services and the `after_up` hook do not run.

#### Scenario: Health check times out
- **WHEN** a service starts but never passes its health check
- **THEN** `devy up` warns that the service is not yet responding, then continues and runs `after_up`

#### Scenario: Service already running
- **WHEN** redis is already running
- **THEN** `devy up` does not start it again

#### Scenario: First up seeds the service
- **WHEN** `postgres` declares `seed: ./db/seed.sql` and has never been seeded
- **THEN** `devy up` applies the seed after postgres is ready and before starting the next service

#### Scenario: Seed failure aborts up
- **WHEN** applying the postgres seed fails
- **THEN** `devy up` fails with `Failed to seed postgresql`, records nothing, and does not run `after_up`

#### Scenario: Unready service skips seeding
- **WHEN** a service that declares `seed` never passes its health check
- **THEN** `devy up` warns that the seed was skipped and continues
