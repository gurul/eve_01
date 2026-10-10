#pragma once
// Person tracking + owner memory glue (spike: past-experiments/firmware/spikes/stackchan_look).
//
// look.cpp runs the GC0308 on a core-0 task and reports a motion/face
// bearing; owner_model.h keeps a LIVE gaze (fresh motion) and a MEMORY of
// where the owner usually sits. gaze.cpp turns those into bodyLookAt()
// requests according to the pet state, and persists the memory in NVS
// (namespace "owner", key "model"). Plain header: no M5 types.
#include <stdint.h>
#include "persona.h"

// After bodyBegin(). Restores the owner model from NVS (prints
// `[owner] restored yaw=.. pitch=.. conf=..` or `[owner] fresh`) and starts
// the camera task. loop() never blocks on the camera.
void gazeBegin();

// Host vision inputs, copied from TamaState by main.cpp each loop (gaze.cpp
// cannot include data.h: that pulls in the pet's board compat layer).
struct GazeHostInput {
  bool     camOn;            // {"cmd":"cam","on":..} — start/stop the frame stream
  uint8_t  camFps;
  uint16_t camW, camH;
  bool     wireBusy;         // xfer transfer active: skip frames
  uint32_t faceSeq;
  int8_t   faceBx, faceBy;   // +bx = right of frame, +by = down
  uint8_t  faceSize, faceConf;
  int16_t  faceYaw, facePitch;   // echoed head pose from the frame line
  bool     faceOwner;        // "who":"owner"
  uint32_t faceAtMs;         // millis() of the last face cmd, 0 = never
  bool     hostLookReq;      // {"cmd":"look"} pending (consumed: set false)
  int16_t  hostLookYaw, hostLookPitch;
  uint16_t hostLookHold;
  bool     explore;          // {"cmd":"mode","explore":true}
  bool     cardUp;           // a permission card is on screen
  bool     agentActive;      // a voice conversation is open ({"cmd":"agent"} not idle): host looks always land
};

// Once per loop after bodyUpdate(). `ownerReset` is data.h's
// {"cmd":"owner","op":"reset"} flag; it is consumed (cleared) here, as is
// host->hostLookReq.
void gazeUpdate(PersonaState active, bool needsAttention, bool listening,
                uint32_t now, bool* ownerReset, GazeHostInput* host);

// What the camera saw since the last call, for the mood engine: the strongest
// on-board motion/skin confidence, and whether a face (host or on-board) was
// seen and whether the host tagged it as the owner. Clears on read.
struct GazeObs { uint8_t motionConf; bool faceSeen; bool faceOwner; };
bool gazeTakeObs(GazeObs* out);
// A touch on the pet is an owner observation (weight 2 at yaw ±28, pitch
// level). Called from bodyNoteToucher().
void gazeNoteTouch(int8_t side);

// The eyes lead the head. The host reports where a face is in each camera frame; the neck takes about a
// second to act on that (follow.py waits for the glide and the frame pipeline), the eyes need not. These
// are the degrees to ADD to the head's own angles when aiming the eyes: twice the face's offset from the
// centre of the frame, so a person 10 degrees off-centre already crosses the eyes' +-20 band and gets a
// glance; 0 when no face has been reported for kEyeLeadFreshMs. Any face, owner or guest.
int8_t gazeEyeLeadYawDeg();
int8_t gazeEyeLeadPitchDeg();
