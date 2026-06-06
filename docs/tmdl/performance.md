# Modelling performance with TMDL

## Defining a machine

Modern SoCs usually include multiple processing units of different architecture.
Those are typically classic general-purpose CPU cores, GPUs, NPUs, DSPs. TIR is
designed to accomodate them all.

A machine in TIR is any processing unit. Machine definitions are split between
TMDL for configurable parts and Rust code for shared pieces. TMDL machine looks
like this:

```
machine GenericInOrder ("generic-in-order") for [TargetIsa] {
  resource ALU { units = 1; }
  resource LSU { units = 1; }

  bind WriteIALU { reads = ID; writes = EX;  uses = [ALU]; }
  bind WriteLoad { reads = ID; writes = MEM; uses = [LSU]; }

  // Bypass network: an ALU result feeds a dependent ALU op back-to-back (0-cycle
  // forward); a load result reaches a dependent ALU op with a 1-cycle bubble.
  forward ALU => ALU { latency = 0; }
  forward LSU => ALU { latency = 1; }
}
```

## Pipelines and resources

TIR machines use classic 5-stage pipeline by default. The stages are:

- `IF` - instruction fetch stage, where machine reads instructions from global
  memory or cache.
- `ID` - instruction decode stage, where ISA instructions are turned into micro-ops.
- `EX` - execution stage that performs actual computation.
- `MEM` - memory stage that does loads or stores.
- `WB` - write-back stage that retires ISA instructions.

Simpler machines (like MCU class cores) can implement less stages. In that case
the missing stages will be merged into a previous execution stage:

```
// Define a 3-stage in-order MCU with no hazards
pipeline {
  IF;
  ID;
  EX;
}
```

Pipeline stages utilize resources that come in two forms: buffers and units.
Buffers (as the name suggests) are arrays or queues of instructions or micro-ops.
Machine describes their default values, but simulators may override those for
performance experiments:

```
machine MyMachine ... {
  buffers {
    rob = 128;
    lsq = 32;
    iq = 64;
  }
}
```


Units are pieces of hardware that perform actual work, like ALU or FPU:

```
unit ALU { count = 2; }
unit LSU { count = 1; }
```

## Defining instruction latencies

Each ISA can define machine-independent scheduling classes - groups of operations
that behave similarly and are expected to be handled by the same hardware units.

```
sched_class WriteIALU { latency = 1; }
sched_class WriteLoad { latency = 3; }
```

Classes can later be bound to units:
 
```
bind WriteIALU { reads = ID; writes = EX; uses = [ALU]; }
bind WriteLoad { reads = ID; writes = MEM; uses = [LSU]; }
```

Real devices may have bypass networks to backpropagate the result of execution
to a previous stage to avoid stalls until instruction retirement. An typical
example in superscalar CPUs would be ALU forwarding:

```
addi x1, x2, 42
add x4, x3, x1
```

Without bypass network the execution of second instruction would stall for 2
cycles for `addi` instruction to reach writeback stage and actually put the
value into register file. With forwarding the execution of `add` can start on
the next cycle. In TMDL this is expressed with the following syntax:

```
forward ALU => ALU { latency = 0; }
forward LSU => ALU { latency = 1; }
```
