# Sub-simulations

An ejected cluster that has drifted away becomes a **sub-simulation** (a *subsystem*). It gets its own engine, of the same type as the simulation, and its own frame of reference, with the origin near its barycenter. Its own tree also keeps precision intact very far from the center, which matters for the single-precision GPU engine.

The composite engine `MultiSystem` (`crates/stellar-application/src/systems.rs`) advances every subsystem with the same step. The rest of the application sees an ordinary `PhysicsEngine`. The simulation thread **always** wraps the engine in a `MultiSystem`. Usually it holds a single subsystem with a zero origin, and the composite only forwards calls.

## Coupling

Each subsystem feels the others through a linear field a(x) = a₀ + T·(x − x_c): the acceleration at its barycenter plus the tidal tensor of point masses (`stellar-domain/src/systems.rs`). The field is computed from the barycenters **extrapolated to the end of the step**, using velocity and mutual attraction (`apply_fields`).

Engines receive it through `PhysicsEngine::set_external_field`. The value passed is the field at the end of the step; the first half kick reuses the field of the previous step, as Leapfrog requires. Tides therefore stretch and strip clusters, and two clusters can orbit each other. The total energy is the sum of the subsystems' own energies and their interaction energies (point sources).

## Order of operations in `MultiSystem::step`

1. If there is more than one subsystem, set the external fields.
2. Step every engine. Read back its moments and translate them to global coordinates.
3. Bodies removed by a **secondary** subsystem (escapees) are handed over to the main subsystem (the most massive one, `root`), whose next cleanup decides their fate. Bodies removed by the main subsystem go into the step report (`StepReport::removed`, `removed_bodies`).
4. On cleanup steps only, the structure operations run in this order:
   - **split**: at most one per subsystem and per cleanup, because rebuilding changes body indices. A cluster becomes a subsystem when (cluster size + system size) < 0.3 × distance, where the size of a cluster is twice its half-mass radius;
   - **merge**, repeated while possible: two subsystems are merged when (sum of sizes) > 0.6 × distance. The hysteresis avoids oscillating between split and merge;
   - **recentring**: a subsystem whose barycenter has moved away from its origin by more than half its size is rebuilt around the barycenter.

   If anything changed, the moments are recomputed and the live view is republished. Clusters that were not split are listed in `StepReport::clusters`.

Structure operations **rebuild the engines from their state** (`build`, which takes bodies in global coordinates). No engine is updated incrementally.

## Invariants

- `state()` returns bodies in **global coordinates**, grouped by subsystem; `SimulationState::systems` gives the range and size of each one. Saves store this grouping (format 5 and later).
- Live view: `SystemsView` holds one `BodiesView` and one origin per subsystem. The renderer translates the camera anchor into each origin.
- Recordings store one quantization box per subsystem in each snapshot (format 3 and later).
- An escapee inserted with `insert` gets no acceleration of its own for its first half kick (a known approximation).

## Known costs

Each small GPU subsystem costs about 0.6 ms per step, for its own submission and wait. Possible improvements: compute subsystems below a few thousand bodies on the CPU, or batch their submissions.
