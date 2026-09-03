# Open audit findings — 3 September 2026

Two Opus audits of `feat/interaction-graph` returned before the session ran
out of credits. **Neither branch is merged and the package has zero production
callers, so nothing here is live.** Both verdicts must be cleared before merge.

---

## A. `24931802` — the audit-fix round. Verdict: FIX FIRST

Seventeen of the earlier findings are genuinely closed (the auditor reproduced
20 original probes). Three of the writer's own headline claims are refuted, and
3 of 12 claim-targeted mutations survive all 52 tests.

1. **HIGH — `reduced_hessian` returns a silently wrong Hessian near a simplex
   face.** `redlich_kister.py:215`, `d = min(step, slack / 4.0)` has no floor.
   At slack 1e-11 the returned Hessian is `[[0,0],[0,0]]` while the truth is
   negative definite, so `unstable_directions` reports **stable** for a point
   unstable in both directions. Finite and symmetric, so `NonFiniteHessian`
   never fires. On an ideal-solution G the relative error saturates at 4.65 %
   and passing a smaller `step` changes nothing, because the code overrides it.
   Interior accuracy is genuinely O(d²); the clamp is the defect. Fix: refuse
   when `slack/4 < 1e-6` by name, or compute at `d` and `d/2` and refuse when
   the Richardson estimate exceeds a set fraction of the norm.
2. **HIGH — `spinodal_intervals` has no branch check**, though the module
   docstring (`redlich_kister.py:7-8`) claims every entry point has one. The
   only gate is `hasattr(free_energy, "temperature_k")` at `:152`. An object
   tagged `CONVEX_ENVELOPE` gets a spinodal computed for it. The hull is still
   differentiable one indirection away, through a caller-asserted
   `ReducedFreeEnergy` tag. `IdealSolutionFreeEnergy.branch` and
   `ConvexEnvelope.branch` are read by nothing in production.
3. **HIGH — clipped spinodal boundaries are returned as if refined.**
   `:169-170`. A grid of `linspace(0.2, 0.8, 51)` returns `[(0.2, 0.8)]`, both
   grid edges, not roots. A descending grid returns `lo > hi`. A single-point
   grid returns a zero-width spinodal. A negative region narrower than the grid
   spacing is reported as convex. Fix: validate and sort the grid; tag each
   boundary refined vs clipped, or refuse a clipped one.
4. **HIGH (test honesty) — surviving mutants.** Deleting the clamp entirely,
   widening it to `slack/2`, keeping collinear hull vertices, swapping the MP
   corrected/uncorrected precedence, and `f < 0` → `f <= 0` all leave 52/52
   green. The clamp survives because its test only probes slack 0 (caught by
   the pre-check) and slack 0.3 (clamp never engages).
5. **MEDIUM — "unstated never compares equal" makes a frame unequal to itself.**
   `assert_one_frame([U])` passes while `assert_one_frame([U, U])` raises
   `FrameMismatch` printing the same frame twice. Dedup and set membership
   become identity-dependent. Fix: check `frame.unstated()` explicitly and
   raise a distinct `UnstatedFrame` instead of routing it through `__eq__`.
6. **MEDIUM — NaN passes every validator.** `abs(nan - 1.0) > 1e-6` is false and
   `nan < 0.0` is false, so NaN atom fractions, energies and `var_h` are all
   accepted and propagate into a fit that returns NaN coefficients with no
   refusal. Fix: `math.isfinite` at construction.
7. **MEDIUM — the fit throws away `var_h` and then reports an uncertainty.**
   The OLS covariance and its degrees of freedom are correct, but per-point
   sampling variance is heteroscedastic by construction and never used. Measured
   on a case with one noisy point: the returned uncertainty is 20× too large and
   the estimate 240× further from truth than weighted least squares. Also
   `residual_rms` divides by n while `sigma2` divides by n−p, so the two
   reported numbers are not comparable.
8. **MEDIUM — `EdgeSurface` enforces nothing after construction.** Public
   mutable fields let a foreign-frame response be appended directly, bypassing
   both the frame check and the configuration-link check, with `revision`
   unchanged.
9. **MEDIUM — MP raw energy silently wins over the corrected one**, contradicting
   the docstring's stated precedence; swapping it passes all 52 tests.
10. **MEDIUM — the "no fifth branch" argument answers the wrong question.** It
    settles the branch question correctly, but what is missing for a homogeneous
    ordered solution is the coordinate `q` the design itself promises. B2 and
    B32 at the same x and T are indistinguishable inside their branch. Below the
    order–disorder temperature the disordered curve is a constrained saddle, and
    a spinodal computed on it is not the physical spinodal, which nothing says.
    `bcc_average_sro` has no branch guard and returns −0.143 for perfect B2
    versus −0.050 for genuinely mild short-range order.
11. **LOW** — absurd residuals returned without a flag; `RedlichKister`
    extrapolates outside [0,1] freely.

## B. `46f70f87` — the DFT collaborator interface. Verdict: DO NOT LAND

The headline safety property is false, and with `requires_approval=False` this
is an unprompted arbitrary-write primitive. The headline correctness property
is opt-in and defaults to off.

1. **BLOCKING — containment is bypassed by the `system` argument.**
   `dft_tools.py:21-27` validates `output_dir` only; `dft_request.py:279` then
   builds `request_id` from caller-supplied element names and `:149-160` joins
   it. `Path` does not normalise `..`. Measured: a call with
   `system=["../../../../VICTIM/OWNED", "W"]` reported success and wrote
   `README.md`, `manifest.json`, a job file and a tarball **outside the project
   directory**. Both filenames are fixed, so this overwrites any `README.md` or
   `manifest.json` the process can write, anywhere, with no prompt.
2. **BLOCKING — the tarball is a tar-slip payload.** Member names contain `..`
   segments verbatim, so extracting it on the collaborator's HPC account writes
   outside the extraction directory. The blast radius reaches a third party.
3. **BLOCKING — `dft_ingest` has no containment at all**, and `:169` joins the
   collaborator-controlled `job["output"]` with no traversal check. Measured: a
   results file outside the results directory was opened, hashed, parsed and
   turned into a record. A results tarball is a file-read primitive.
4. **BLOCKING — the frame refusal is opt-in and defaults to self-certification.**
   `request_dir` is optional; without it the declared frame id is compared to
   itself. Measured: a run at cutoff 100 eV instead of 520, with a matching
   declared id, was accepted and stamped screening.
5. **HIGH — `requires_approval=False` is wrong on the merits.** `base.py:36-42`
   says declare True for anything that touches the filesystem, and the closest
   in-repo analogue (`app/tools/system.py`) is gated despite stronger
   containment. Given 1 to 3 the trade the code claims to make is not made.
6. **HIGH — surviving mutant on the central invariant.** When a reference state
   is missing, making the collaborator's claimed mixing energy become PRISM's
   derived value leaves 16/16 green; the test only covers the branch where both
   references exist. Nor is a missing reference a refusal at derive time.
7. **MEDIUM** — `frame_id` omits `sqs_atoms` and silently drops unknown settings
   keys, so 16-atom and 128-atom SQS are declared same-frame; `functional=None`
   is stringified to `"None"`, defeating the unstated rule; `code` and
   `code_version` are collaborator claims filed beside observations; the
   manifest that decides the frame is never hashed; malformed manifests raise
   raw `KeyError` and malformed VASP output raises raw parser exceptions rather
   than the named taxonomy; duplicate job ids are accepted silently; the derived
   value is separated from its frame and stamp; a refused request still leaves
   files behind; `binary_grid` has no cap (`grid_step=1e-6` is a million
   compositions) and `grid_step=0` raises `ZeroDivisionError`.
8. **LOW** — two more surviving mutants (schema check, error-result evidence
   class); schemas accept undeclared arguments silently; the parser injection
   seam is production API with only test callers, so the real VASP path has zero
   coverage; the marketplace filesystem-tool denylist was not extended.

**Confirmed clean in both audits:** no MARC27 or facility resource in source, no
owner research file touched, evidence class not self-certified and not
agent-settable, the full-suite failure set is byte-identical to the parent.

---

## Order of work when credits return

1. DFT interface findings 1 to 5 — the security ones, before anything else.
2. Fix-round findings 1 to 4 — the wrong-stability-verdict one first.
3. Re-audit both, twice, as before.
