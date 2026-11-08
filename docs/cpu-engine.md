# CPU engine

`crates/stellar-engine-cpu`: multithreaded Barnes-Hut in double precision (rayon, AVX2 / FMA).

At each step:

1. **Kick ½ dt**: half update of the velocities.
2. **Drift dt**: update of the positions; start positions are kept.
3. **Swept contacts**:
   - hierarchical grid over the bounding boxes of the trajectories; very fast or very large bodies go to the upper levels;
   - neighbouring cells are found without a hash table: the sorted cell keys are walked with one cursor per direction;
   - exact minimum distance between straight trajectories;
   - chained mergers grouped by union-find.
4. **Merger**: mass and momentum are conserved and the position moves to the barycenter. The identifier of the most massive member is kept.
5. **Forces**:
   - bodies reordered by Morton code;
   - octree built in parallel, stored in pre-order and walked without a stack;
   - opening criterion `d > s/θ + δ` and Plummer softening;
   - **grouped traversal** of at most 32 neighbouring bodies: the tree is walked once per group, using the group's box for the criterion, then the interaction list is evaluated for each body. With AVX2 and FMA, four bodies are processed per instruction, and the inverse square root uses a Newton-refined estimate.
6. **Kick ½ dt**.
7. **Cleanup, every N steps**: removal of the bodies that are both **unbound** (positive specific energy relative to the barycenter) **and** beyond k × R, **except ejected clusters**. The rule is shared by both engines (`stellar-domain/src/ejection.rs`):
   - far bodies are grouped by "friends of friends" (link length);
   - each group is pruned: members not bound to the group are removed, most energetic first. Energy is measured in the group's frame, under the group's own potential: exactly up to 2,048 members, with spherical symmetry beyond;
   - a bound group of at least n bodies, or of at least a given share of the initial mass, is kept whole even if it moves away indefinitely. A single very massive body counts as a group.

   The GPU engine finds the far bodies on the card and reads back only those, on cleanup steps only.

The computation uses every core but one, which is left to the interface.
