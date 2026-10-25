# Babble Frame Runtime (vendored subset)

The eye-camera client and eye model path from
[Project-Babble/Babble-Frame-Runtime](https://github.com/Project-Babble/Babble-Frame-Runtime) at
`e96a3d31d945ae6d7b17cecd5455a1f1052d6c5e` (V1.1), GPL-3.0-or-later, kept at their original paths.

Changes from upstream:

- `core/log.h` and `core/clock.h` are small stand-ins (spdlog, `CLOCK_MONOTONIC`).
- `model/postprocess/one_euro.*`: `one_euro_params()` (JSON options) removed.

The eye kit's models (`eye_kit_stage1_fp16.pte`, `eye_kit_stage2_fp16.pte`, same repository and license) are
downloaded by the headset build, see the justfile's `_fetch-eye-model`.
