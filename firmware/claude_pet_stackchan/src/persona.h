#pragma once
#include <stdint.h>
// Persona states shared by the render loop (main.cpp) and the robot body
// choreography (body.cpp). Order matches the species state tables and the
// character GIF manifest: 0=sleep .. 6=heart. Moved out of main.cpp for the
// StackChan port so body.h can take a PersonaState without including the
// board compat layer.
enum PersonaState { P_SLEEP, P_IDLE, P_BUSY, P_ATTENTION, P_CELEBRATE, P_DIZZY, P_HEART };

// The host's voice/computer-control conversation ({"cmd":"agent","state":..}).
// The daemon does the work; the robot acts as if it were: face the owner to
// listen, glance aside to think, bob while speaking, head down at the desk
// with quick eyes while working, head up for a question, a nod on done, a
// wince on error. AG_IDLE = no conversation.
enum AgentState : uint8_t { AG_IDLE = 0, AG_WAKE, AG_LISTENING, AG_THINKING, AG_SPEAKING, AG_WORKING,
                            AG_ASKING, AG_DONE, AG_ERROR };
static const char* const AGENT_STATE_NAMES[] = { "idle", "wake", "listening", "thinking", "speaking", "working",
                                                 "asking", "done", "error" };
inline AgentState agentStateFrom(const char* s) {
  if (!s) return AG_IDLE;
  for (uint8_t i = 0; i < 9; i++) {
    const char* n = AGENT_STATE_NAMES[i]; uint8_t j = 0;
    while (n[j] && s[j] == n[j]) j++;
    if (!n[j] && !s[j]) return (AgentState)i;
  }
  return AG_IDLE;
}
