## Summary

The change adds a retry loop around the store write, but one path retries without a bound and a second swallows the error it retries on.

**Agent assessment:** Fail

## Findings

### 1. Critical

**Location:** /work/demo/src/retry.rs:41-58

**Problem:** `write_with_retry` loops while the store answers `Busy` and has no attempt limit. Trigger: a writer that holds the RocksDB lock for longer than the caller waits (a second `ikigai-gonk` on the same store) spins this loop forever at 100% CPU.

**Fix:** Bound the loop (five attempts with backoff) and return the last `Busy` to the caller.

**Reported by:** codex, claude-code

### 2. High

**Location:** src/retry.rs:72

**Problem:** On the final attempt the error is logged and `Ok(())` is returned.

The caller therefore records a write that never happened. Trigger: any store refusal on the third attempt.

**Fix:** Return the error from the final attempt instead of `Ok(())`.

**Reported by:** codex

### 3. Medium

**Problem:** The retry delay is a fixed 100 ms, so two writers retrying together collide on every attempt.

**Fix:** Add jitter to the delay.

**Reported by:** claude-code

### 4. Low

**Location:** `src/retry.rs:12`

**Problem:** The doc comment says "retries three times" and the constant is 4.

**Fix:** Make the comment name the constant.

**Reported by:** codex
