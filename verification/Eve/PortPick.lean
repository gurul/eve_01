/-
  Which USB node the robot link opens (bridge/src/eve_bridge/serial_transport.py, `_resolve_port`).

  Two ESP32-S3 boards are on USB: the StackChan robot and the Voice PE controller (controller.py). Both
  report USB VID 0x303A (`_ESP32S3_VID`) and differ only by USB serial number (an ESP32-S3's is its MAC).
  The robot link opens the glob `/dev/cu.usbmodem*`; the controller link opens `usbsn:<serial>`
  (controller.py `Controller.__init__`, `USB_SERIAL_PREFIX + serial_number`). The daemon passes the
  controller's serial as the robot's `skip_serials` (daemon.py `Daemon.__init__`,
  `skip_serials=frozenset({ctl_serial}) if ctl_serial else frozenset()`, `ctl_serial` from
  `controller_serial()`, i.e. `EVE_CONTROLLER_SERIAL`, stripped, `None` when empty).

  The bug. Before 2026-09-26 `_resolve_port` had no skip: among the glob's matches it returned the first
  Espressif node in `sorted()` order. On 2026-09-26 that was live: the daemon opened the Voice PE
  (`/dev/cu.usbmodem101`) as the robot while the StackChan (`/dev/cu.usbmodem31201`) sat idle, because
  "101" sorts before "31201". `current_violates` is that trace.

  The property (`fixed_invariant`), for every node list and every skip set:
    (a) the glob's pick is never a node whose USB serial is in skip;
    (b) the glob's pick, when there is one, is one of the nodes;
    (c) a `usbsn:s` pick, when there is one, is a node whose serial is s.
  `fixed_picks_espressif`: when an Espressif node in the glob is not skipped, the glob picks an Espressif node.

  Replay: `test_two_esp32_boards_are_told_apart_by_usb_serial` in bridge/tests/test_serial_transport.py runs
  the same four nodes through the real `_resolve_port` (the controller sorts first with no skip; the robot
  with the controller's serial skipped; each board by `usbsn:`).

  Abstraction.
  - A node is one entry: its device name, whether the glob matches it (`inGlob`), whether
    `list_ports.comports()` lists it (`listed`), whether its name has "/cu." (`cu`), its VID and its serial.
    Device names are distinct (one comports entry per device), so the Python's "any port with this device
    has a skipped serial" is the node's own serial.
  - Device names are Nat, ordered as numbers; `sorted()` orders the Python strings. The two agree on the
    live trace (101 < 31201, "101" < "31201"). (a)-(c) and `fixed_picks_espressif` hold for any order.
  - Serials are already normalised: the Python lower-cases the wanted serial (`.strip().lower()`), every
    port's serial and every skip entry (`.lower()`); the model compares them as given.
  - `(p.serial_number or "")` is `serial.getD ""`. A glob match that comports does not list has no VID and
    is never skipped (`vids.get(d)` is None; no port matches it in the skip filter).

  Not modelled.
  - A pattern with no "*" is returned as is.
  - An exception from `list_ports.comports()`. In the glob branch the Python then falls back to the glob
    alone: the skip filter sits inside the `try`, so it does not run, and `hits[0]` is returned, which can be
    the controller. (a) holds only when enumeration succeeds. In the `usbsn:` branch an exception returns
    None, which (c) allows.
-/
namespace Eve.PortPick

/-- `_ESP32S3_VID`, serial_transport.py. -/
def espVid : Nat := 0x303A

structure Node where
  dev : Nat                  -- the device name, `/dev/cu.usbmodem<dev>`
  inGlob : Bool := true      -- `glob.glob(pattern)` returns it
  listed : Bool := true      -- `list_ports.comports()` lists it
  cu : Bool := true          -- `"/cu." in p.device`
  vid : Option Nat           -- `p.vid`
  serial : Option String     -- `p.serial_number`, already lower-cased
  deriving DecidableEq, Repr

/-- The VID `vids.get(d)` sees: none for a node comports does not list. -/
def vidOf (n : Node) : Option Nat := if n.listed then n.vid else none

/-- The skip filter: a listed node whose `(p.serial_number or "").lower()` is in `skip`. -/
def skipped (skip : List String) (n : Node) : Bool := n.listed && decide (n.serial.getD "" ∈ skip)

/-- `sorted()` by device name (insertion sort). -/
def ins (n : Node) : List Node → List Node
  | [] => [n]
  | m :: ms => if n.dev ≤ m.dev then n :: m :: ms else m :: ins n ms

def sortDev : List Node → List Node
  | [] => []
  | n :: ns => ins n (sortDev ns)

theorem mem_ins {a n : Node} : ∀ {l : List Node}, a ∈ ins n l → a = n ∨ a ∈ l
  | [], h => by simp [ins] at h; exact .inl h
  | m :: ms, h => by
      unfold ins at h
      split at h
      · simpa using h
      · simp only [List.mem_cons] at h
        rcases h with h | h
        · exact .inr (by simp [h])
        · rcases mem_ins h with h | h
          · exact .inl h
          · exact .inr (by simp [h])

theorem mem_ins_of {a n : Node} : ∀ {l : List Node}, (a = n ∨ a ∈ l) → a ∈ ins n l
  | [], h => by simpa [ins] using h
  | m :: ms, h => by
      unfold ins
      split
      · simpa using h
      · simp only [List.mem_cons] at h ⊢
        rcases h with h | h | h
        · exact .inr (mem_ins_of (.inl h))
        · exact .inl h
        · exact .inr (mem_ins_of (.inr h))

theorem mem_sortDev_of {a : Node} : ∀ {l : List Node}, a ∈ l → a ∈ sortDev l
  | [], h => by simp at h
  | n :: ns, h => by
      simp only [sortDev]
      simp only [List.mem_cons] at h
      rcases h with h | h
      · exact mem_ins_of (.inl h)
      · exact mem_ins_of (.inr (mem_sortDev_of h))

theorem mem_sortDev {a : Node} : ∀ {l : List Node}, a ∈ sortDev l → a ∈ l
  | [], h => by simp [sortDev] at h
  | n :: ns, h => by
      simp only [sortDev] at h
      rcases mem_ins h with h | h
      · simp [h]
      · exact List.mem_cons_of_mem _ (mem_sortDev h)

/-- The glob branch after the ESP-VID and camera rules, serial_transport.py `_resolve_port`, lines 144-172.
  `hits` is sorted; `s3` is sorted, so its first element is the first Espressif node of `hits`. -/
def pickFrom (hits : List Node) : Option Node :=
  match hits.find? (fun n => vidOf n == some espVid) with
  | some n => some n                                                  -- `return s3[0]`
  | none =>
      if hits.all (fun n => (vidOf n).isSome) then none               -- every match names a vendor
      else hits.head?                                                 -- `return hits[0]`

/-- The fixed glob branch: sort (144), none if empty (145-146), drop the skipped (150-154), then pick. -/
def globFix (skip : List String) (nodes : List Node) : Option Node :=
  let hits := sortDev (nodes.filter (·.inGlob))
  if hits.isEmpty then none
  else
    let hits := hits.filter (fun n => !skipped skip n)
    if hits.isEmpty then none else pickFrom hits

/-- The glob branch before 2026-09-26: no skip. -/
def globCur (nodes : List Node) : Option Node :=
  let hits := sortDev (nodes.filter (·.inGlob))
  if hits.isEmpty then none else pickFrom hits

/-- The `usbsn:` branch, lines 134-139: the first comports entry with that serial and "/cu." in its name. -/
def usbsn (s : String) (nodes : List Node) : Option Node :=
  nodes.find? (fun n => n.listed && n.cu && n.serial.getD "" == s)

/-! ## The current code -/

def controller : Node := { dev := 101, vid := some espVid, serial := some "0a:00:00:00:00:02" }
def robot : Node := { dev := 31201, vid := some espVid, serial := some "0a:00:00:00:00:01" }
def camera : Node := { dev := 1105, vid := some 0x3564, serial := none }
def dock : Node := { dev := 1, vid := some 0x291A, serial := none }    -- `usbmodemSN1`

/-- 2026-09-26: the camera, the controller, the robot and a dock match the glob. The robot's link picks the
controller, whose serial is the one the daemon now skips. -/
theorem current_violates :
    globCur [camera, controller, robot, dock] = some controller ∧
      skipped ["0a:00:00:00:00:02"] controller = true := by
  decide

/-! ## The fixed code -/

theorem pickFrom_mem {hits : List Node} {n : Node} (h : pickFrom hits = some n) : n ∈ hits := by
  unfold pickFrom at h
  split at h
  · rename_i m hm; cases h; exact List.mem_of_find?_eq_some hm
  · split at h
    · cases h
    · exact List.mem_of_mem_head? h

theorem globFix_sound {skip : List String} {nodes : List Node} {n : Node} (h : globFix skip nodes = some n) :
    n ∈ nodes ∧ skipped skip n = false := by
  unfold globFix at h
  simp only at h
  split at h
  · cases h
  · split at h
    · cases h
    · have hm := pickFrom_mem h
      rw [List.mem_filter] at hm
      have hn := mem_sortDev hm.1
      rw [List.mem_filter] at hn
      exact ⟨hn.1, by simpa using hm.2⟩

/-- For every node list and every skip set: (a) the glob never picks a skipped serial; (b) its pick is a
node on the bus; (c) a `usbsn:s` pick has serial s. -/
theorem fixed_invariant (skip : List String) (nodes : List Node) (s : String) :
    (∀ n, globFix skip nodes = some n → ¬ (n.listed = true ∧ n.serial.getD "" ∈ skip)) ∧
    (∀ n, globFix skip nodes = some n → n ∈ nodes) ∧
    (∀ n, usbsn s nodes = some n → n ∈ nodes ∧ n.serial.getD "" = s) := by
  refine ⟨?_, ?_, ?_⟩
  · intro n h ⟨hl, hs⟩
    have := (globFix_sound h).2
    simp [skipped, hl, hs] at this
  · intro n h; exact (globFix_sound h).1
  · intro n h
    refine ⟨List.mem_of_find?_eq_some h, ?_⟩
    have := List.find?_some h
    simp at this
    exact this.2

/-- When an Espressif node in the glob is not skipped, the glob picks an Espressif node. -/
theorem fixed_picks_espressif (skip : List String) (nodes : List Node) (e : Node)
    (he : e ∈ nodes) (hg : e.inGlob = true) (hv : vidOf e = some espVid) (hs : skipped skip e = false) :
    ∃ n, globFix skip nodes = some n ∧ vidOf n = some espVid := by
  have hsorted : e ∈ sortDev (nodes.filter (·.inGlob)) := mem_sortDev_of (List.mem_filter.2 ⟨he, hg⟩)
  have hmem : e ∈ (sortDev (nodes.filter (·.inGlob))).filter (fun n => !skipped skip n) :=
    List.mem_filter.2 ⟨hsorted, by simp [hs]⟩
  have hne1 : (sortDev (nodes.filter (·.inGlob))).isEmpty = false :=
    List.isEmpty_eq_false_iff_exists_mem.2 ⟨e, hsorted⟩
  have hne2 : ((sortDev (nodes.filter (·.inGlob))).filter (fun n => !skipped skip n)).isEmpty = false :=
    List.isEmpty_eq_false_iff_exists_mem.2 ⟨e, hmem⟩
  unfold globFix
  simp only [hne1, hne2, Bool.false_eq_true, ite_false]
  unfold pickFrom
  split
  · rename_i m hm
    exact ⟨m, rfl, by simpa using List.find?_some hm⟩
  · rename_i hm
    have := List.find?_eq_none.1 hm e hmem
    simp [hv] at this

/-- The live trace on the fixed code: with the controller's serial skipped the robot is picked, and each
board is found by its serial. -/
theorem fixed_replays_counterexample :
    globFix ["0a:00:00:00:00:02"] [camera, controller, robot, dock] = some robot ∧
      usbsn "0a:00:00:00:00:02" [camera, controller, robot, dock] = some controller ∧
      usbsn "0a:00:00:00:00:01" [camera, controller, robot, dock] = some robot := by
  decide

end Eve.PortPick
