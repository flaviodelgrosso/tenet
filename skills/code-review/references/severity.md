# Severity calibration

Severity reflects impact and likelihood, not reviewer preference.

## Critical

Use only for realistic catastrophic outcomes such as severe compromise, irreversible/corrupting data loss, or systemic outage without practical mitigation.

Blocking.

## High

Use for concrete defects likely to affect normal/important usage, violate a required contract, compromise meaningful security boundaries, corrupt data, or materially destabilize the system.

Blocking by default.

## Medium

Use for real defects with narrower triggers, moderate reliability/maintainability cost, or meaningful missing regression protection.

Normally non-blocking unless project policy says otherwise.

## Low

Use for small, concrete improvements with limited impact. Never use low severity to encode personal style preferences.

## Tie-break rule

When uncertain between two severities, choose the lower one unless evidence supports the higher impact.
