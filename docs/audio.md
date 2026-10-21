# Audio

Files: `kernel/src/virtio/sound.rs` (driver), `kernel/src/audio/` (mixer, /dev/audio,
system sounds, WAV decoding), `user/play`, `scripts/gen_sounds.py` (system sounds, outputs
in `rootfs/System/Library/Sounds/`), `scripts/audio-test.sh` + `scripts/wav_check.py`.

## Running it

| | |
|---|---|
| `scripts/run.sh` | **no sound device by default** |
| `MILK_SOUND=1 scripts/run.sh` | virtio-sound through the Mac's speakers (coreaudio). For a human at the Mac only. |
| `MILK_SOUND_WAV=/tmp/x.wav scripts/run.sh` | records to a WAV instead (silent) |
| `scripts/audio-test.sh [out.wav]` | the end-to-end test, wav backend only: never makes a sound |

qemu args (tests): `-audiodev wav,id=snd0,path=FILE,out.frequency=48000 -device
virtio-sound-device,audiodev=snd0` (`out.frequency` keeps qemu from resampling to its
44.1 kHz default). qemu's "Could not create a backend for voice virtio-sound.in" warning
is the capture stream the wav backend can't do; harmless (we never capture). Note the wav
backend only writes while a stream plays: silence between sounds is not in the file.

## Format

One format, everywhere: **S16_LE, 48000 Hz, 2 channels, interleaved** (4 bytes per frame).
Writers convert (`play` does, for WAV files of any rate, mono or stereo).

## Driver (`virtio/sound.rs`)

Device 25 over virtio-mmio. Queues: control (0), event (1, 4 buffers, xruns counted), tx
(2); rx is not set up. At attach: `PCM_INFO` for all streams, pick the first output stream
with S16 / 48 kHz / 2 channels, `SET_PARAMS` (period 1920 bytes = 10 ms, buffer 4 periods),
`PREPARE`. A tx request is `virtio_snd_pcm_xfer` (stream id, 4 bytes) → one period of PCM →
`virtio_snd_pcm_status` (device-writable, 8 bytes); the device completes it when the period
has been played, so completions pace the mixer. 8 period slots, at most 4 in flight.
Control requests are polled (1 s timeout) under the driver lock; the IRQ handler only acks
and wakes the mixer thread. API used by `audio`: `with(|d| ...)`, `submit(fill)`,
`can_submit`, `start`, `stop`, `reclaim`, `in_flight`, `running`, counters.

## Mixer (`audio/mod.rs`)

* **Clients**: every process writing `/dev/audio` gets one (keyed by pid; forgotten 2 s
  after its last write once empty), with a 250 ms ring. Each system sound gets its own
  client too. So two writers never trample each other: they are mixed.
* **The "audio" thread** (High priority): mixes one 10 ms period from all clients
  (saturating i32 sum, clipped samples counted), submits it, as long as the device has
  room. It starts the stream once the device buffer is full (or the sound is shorter),
  and stops it 200 ms after everything has drained. It sleeps on a wait queue (woken by the
  device IRQ, new data, system sound requests), with a 50 ms timeout while playing.
* **Writers** block while their ring is full (sleeping, woken as the mixer consumes).

### `/dev/audio` (char 14,4)

* `write`: PCM in the format above (partial frames carried over between writes). Blocks
  until queued, not until played.
* `ioctl(SNDCTL_DSP_SYNC = 0x5001)`: wait until this process's data has been played (its
  last period came back from the device; other writers may still be playing).
* `ioctl(MILK_AUDIO_SYSTEM = 0x4d01, n)`: play system sound n (0 notify, 1 error, 2 chime, 3 shutter).
* `read`: `EINVAL` (no capture). `poll`: POLLOUT while the writer's ring has room.

### System sounds (for the desktop)

```rust
crate::audio::play_system(crate::audio::Sound::Notify);   // or ::Error, ::Chime, ::Shutter
crate::audio::available() -> bool                         // is there a sound device?
```

Non-blocking and safe from any thread (not from IRQ handlers: it takes a spinlock, which
is fine there too, but there's no reason to). The mixer thread loads
`/System/Library/Sounds/{notify,error,chime}.wav` on first use (cached), decodes them (any
16-bit PCM WAV; mono/rate converted) and mixes them in. Without a sound device it does
nothing. The wm hook is not wired: that's the wm side's call.

### `/proc/audio`

Format, clients and how much each has queued, frames mixed, stream starts, clipped
samples, system sounds played, device state, periods in flight / played, xruns, control
errors.

## Userland

* `play tone FREQ SECS [VOLUME]`: sine with 10 ms ramps (VOLUME 0..1, default 0.3).
* `play chime`: the boot chime (G5 then C6, soft attack, exponential decay, ~0.95 s).
* `play FILE.wav`: 16-bit PCM WAV, mono or stereo, any rate (linear resampling).
* `play system notify|error|chime`: through the kernel's `play_system`.
* `play -n ...`: don't wait for playback to finish (default: SNDCTL_DSP_SYNC before exit).
* init plays the chime at boot (`play -n chime`, in the background) when `/dev/audio`
  exists.

`sin` is computed with a short Taylor series (user programs are `no_std`, no libm).

## System sounds (`scripts/gen_sounds.py`)

Generated with the python3 stdlib, committed with its outputs (48 kHz mono 16-bit):
`notify.wav` (A5 → E6, 0.4 s), `error.wav` (G4 → Eb4, 0.45 s), `chime.wav` (the boot chime,
0.95 s). Rerun the script after changing it.

## Test (`scripts/audio-test.sh`)

Boots with the wav backend, lets init play the chime, types into msh: `play tone 440 0.5`,
`play tone 1000 0.25`, `play tone 660 1.2 0.25 &` + `play tone 440 0.4 0.25` (two writers),
`play system notify`, `play /System/Library/Sounds/error.wav`, `cat /proc/audio`, stops
qemu gracefully (it writes the WAV header on exit) and checks the file with
`wav_check.py`: 48 kHz stereo; chime G5 then C6; 440 Hz for 0.48 s (+ramps); 1000 Hz for
0.24 s; a 660 Hz stretch with 440 Hz mixed in (100 ms Goertzel windows: both at -12 dB, then
660 alone); notify's A5 + E6; error's G4 + Eb4; no clipping.

`scripts/wav_check.py FILE [--probe F,...] [--json]` on its own prints the timeline of any
16-bit WAV (start, length, zero-crossing pitch, level, optional Goertzel levels).

## Not done

Capture (rx), volume control / mixer ioctls, other formats or rates (conversion is the
writer's job), jacks and channel maps, a real `SNDCTL_DSP_*` OSS surface, sample-accurate
latency reporting.
