// SPDX-License-Identifier: GPL-3.0-or-later
#pragma once

/// Writes what this Mac plays as JSON lines on stdout, one each time it changes, until stdin closes. Never returns.
/// Called from /usr/bin/perl (see NowPlaying.swift): since macOS 15.4 MediaRemote answers only Apple's own
/// processes, and the system perl is one of them.
void luch_now_playing(void *perl, void *cv);
