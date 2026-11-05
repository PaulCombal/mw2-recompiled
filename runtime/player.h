#pragma once

#include <cstdint>
#include "diagnostics.h"

// Where the player stands, and how to send them somewhere else.
//
// A defect that only appears at one place on one map cannot be reproduced by a
// run that spawns and walks at random, and the multiplayer refuses the console
// commands that would teleport: `setviewpos`, `noclip` and `kill` are registered
// client-side with a null handler, forwarded to the server, and dropped there.
// What is left is to play the game: read where the player is, work out which way
// the target lies, and hold the sticks that get them there. Diagnostics only.
#if MW2_DIAGNOSTICS
namespace player { struct Where; }
extern "C"
{
    bool mw2_player_wanted();
    bool mw2_player_locate(player::Where* out);
    bool mw2_player_steer(short* axes, int* press);
    bool mw2_player_walking();
    void mw2_player_record();
    void mw2_player_saw_pad(uint16_t buttons, uint8_t rightTrigger);
    bool mw2_player_arrived();
    double mw2_player_seconds_since_progress();
    void mw2_player_report();
}
#endif

namespace player
{
    struct Where
    {
        float x = 0, y = 0, z = 0;
        float yaw = 0, pitch = 0;
    };

    // What the autopilot wants pressed this poll: A, and Up only when asked for
    // on the team selection (MW2_TEAM_UP); see DismissMenus. Left and Fire (the
    // right trigger) only where a recorded route says the player did so.
    enum class Press { None, A, Up, Left, Fire };

#if MW2_DIAGNOSTICS

    // Whether anything here is switched on, so the input path knows to run even
    // with no pad attached and no script holding a stick.
    inline bool Wanted() { return mw2_player_wanted(); }

    // False before the player exists -- the menus, the loading screen, and the
    // seconds between spawning and the first snapshot.
    inline bool Locate(Where& out) { return mw2_player_locate(&out); }

    // MW2_WALK_TO="x,y[,radius[,yaw]]" and MW2_WALK_PATH=<route>[,<route>...]
    // drive the player: turn until the next corner or the target is ahead, then
    // walk, and on arrival either hold the yaw given or turn slowly on the spot.
    // `axes` is the guest's stick state, taken over only while there is somewhere
    // to go and the player has been located, and `press` is the button to hold
    // this poll. Returns true if it wrote anything.
    inline bool Steer(short axes[4], Press& press)
    {
        int pressed = 0;
        const bool wrote = mw2_player_steer(axes, &pressed);
        press = Press(pressed);
        return wrote;
    }

    // Whether there is anywhere to go at all, and whether the player is standing
    // at it. A diagnostic that wants "what does this look like *there*" gates on
    // these rather than on a frame number, which is a guess about how long the
    // walk took and is usually wrong.
    inline bool Walking() { return mw2_player_walking(); }
    // MW2_RECORD_PATH=<file> writes down where the player goes while someone
    // plays, one point every MW2_RECORD_SPACING units (110 by default), as
    // `x y z yaw pitch` a line. MW2_WALK_PATH=<file> walks that route back. A route
    // someone actually walked is walkable; a handful of points pressed at
    // corners is not, which is how the replays kept ending up inside walls.
    //
    // The route also keeps what the player did with the pad on the way: a
    // `left` line where d-pad left went down (the grenade launcher), a `fire`
    // line where the right trigger was pulled, each with the place and the aim
    // -- `x y z yaw pitch`. The replay walks to each, turns to its aim, and
    // does the same; so a crash that takes a shot at one spot can be replayed
    // without a hand on the pad. Every pull is logged, recording or not.
    inline void Record() { mw2_player_record(); }
    // The pad's own buttons and right trigger for user 0, every poll, before
    // the autopilot adds anything.
    inline void SawPad(uint16_t buttons, uint8_t rightTrigger) { mw2_player_saw_pad(buttons, rightTrigger); }

    inline bool Arrived() { return mw2_player_arrived(); }
    // Seconds since the walk last got meaningfully nearer its target. A run that
    // has stalled against a wall is finished, whatever the watchdog says.
    inline double SecondsSinceProgress() { return mw2_player_seconds_since_progress(); }

    // For the shutdown report: whether the target was reached, and where the run
    // actually spent its time.
    inline void Report() { mw2_player_report(); }
#else
    inline bool Wanted() { return false; }
    inline bool Locate(Where&) { return false; }
    inline bool Steer(short[4], Press&) { return false; }
    inline bool Walking() { return false; }
    inline void Record() {}
    inline void SawPad(uint16_t, uint8_t) {}
    inline bool Arrived() { return false; }
    inline double SecondsSinceProgress() { return 0; }
    inline void Report() {}
#endif
}
