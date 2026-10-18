# Babble Frame Runtime (vendored subset)

The eye-camera client and eye model path from
[Project-Babble/Babble-Frame-Runtime](https://github.com/Project-Babble/Babble-Frame-Runtime) at
`511bf3772568224b95b0aa21875e163dd10f4ec5`, GPL-3.0-or-later, kept at their original paths.

Changes from upstream:

- `core/log.h` and `core/clock.h` are small stand-ins (spdlog, `CLOCK_MONOTONIC`).
- `model/postprocess/one_euro.*`: `one_euro_params()` (JSON options) removed.

The model (`eye_split96_uncapped_fp16.pte`, same repository and license) is downloaded by the headset build, see
`scripts/fetch-eye-model.sh`.
