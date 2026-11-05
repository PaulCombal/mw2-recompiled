#pragma once
#include "diagnostics.h"
// The predicate waits every engine stall ends in (src/hooks/predicate_waits.rs,
// predicate_waits.cpp in the C++ build).
#if MW2_DIAGNOSTICS
extern "C" void mw2_engine_report_predicate_waits();
#endif

namespace engine
{
#if MW2_DIAGNOSTICS
    inline void ReportPredicateWaits() { mw2_engine_report_predicate_waits(); }
#else
    inline void ReportPredicateWaits() {}
#endif
}
