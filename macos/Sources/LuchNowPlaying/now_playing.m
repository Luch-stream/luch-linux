// What this Mac plays, from the private MediaRemote framework, as JSON lines (PROTOCOL.md 4d, now_playing).
// SPDX-License-Identifier: GPL-3.0-or-later
#import <AppKit/AppKit.h>
#include <dlfcn.h>
#include "luch_now_playing.h"

typedef void (*GetInfo)(dispatch_queue_t, void (^)(NSDictionary *));
typedef void (*GetPID)(dispatch_queue_t, void (^)(int));
typedef void (*GetPlaying)(dispatch_queue_t, void (^)(Boolean));
typedef void (*Register)(dispatch_queue_t);

static GetInfo getInfo;
static GetPID getPID;
static GetPlaying getPlaying;
static NSData *lastArt;
static NSString *lastLine;

static id orNull(id v) { return v ?: [NSNull null]; }

/// The artwork goes to a file, not into the line: a cover is tens of kilobytes and changes once a track.
/// A new cover gets a new name, so the sender can tell it changed without reading it.
static NSString *artPath(NSData *art) {
    static NSString *path;
    static unsigned n;
    if (art.length == 0) return nil;
    if (![art isEqualToData:lastArt]) {
        if (path) [[NSFileManager defaultManager] removeItemAtPath:path error:nil];
        NSString *name = [NSString stringWithFormat:@"luch-now-playing-%d-%u", getpid(), ++n];
        path = [NSTemporaryDirectory() stringByAppendingPathComponent:name];
        [art writeToFile:path atomically:YES];
        lastArt = art;
    }
    return path;
}

static void emit(void) {
    getInfo(dispatch_get_main_queue(), ^(NSDictionary *info) {
        getPID(dispatch_get_main_queue(), ^(int pid) {
            getPlaying(dispatch_get_main_queue(), ^(Boolean playing) {
                NSMutableDictionary *o = [NSMutableDictionary dictionary];
                o[@"title"] = info[@"kMRMediaRemoteNowPlayingInfoTitle"];
                o[@"artist"] = info[@"kMRMediaRemoteNowPlayingInfoArtist"];
                o[@"album"] = info[@"kMRMediaRemoteNowPlayingInfoAlbum"];
                o[@"dur"] = info[@"kMRMediaRemoteNowPlayingInfoDuration"];
                o[@"elapsed"] = info[@"kMRMediaRemoteNowPlayingInfoElapsedTime"];
                o[@"rate"] = info[@"kMRMediaRemoteNowPlayingInfoPlaybackRate"];
                NSDate *at = info[@"kMRMediaRemoteNowPlayingInfoTimestamp"];
                if ([at isKindOfClass:[NSDate class]]) o[@"at"] = @(at.timeIntervalSince1970);
                o[@"art"] = artPath(info[@"kMRMediaRemoteNowPlayingInfoArtworkData"]);
                o[@"art_mime"] = info[@"kMRMediaRemoteNowPlayingInfoArtworkMIMEType"];
                o[@"app"] = [NSRunningApplication runningApplicationWithProcessIdentifier:pid].localizedName;
                o[@"playing"] = @(playing != 0);
                o[@"present"] = @(info.count > 0);
                for (NSString *k in o.allKeys) {
                    if (![NSJSONSerialization isValidJSONObject:@[ o[k] ]]) [o removeObjectForKey:k];
                }
                NSData *json = [NSJSONSerialization dataWithJSONObject:o options:NSJSONWritingSortedKeys error:nil];
                NSString *line = [[NSString alloc] initWithData:json encoding:NSUTF8StringEncoding];
                if (line == nil || [line isEqualToString:lastLine]) return;
                lastLine = line;
                printf("%s\n", line.UTF8String);
                fflush(stdout);
            });
        });
    });
}

void luch_now_playing(void *perl, void *cv) {
    (void)perl;
    (void)cv;
    void *mr = dlopen("/System/Library/PrivateFrameworks/MediaRemote.framework/MediaRemote", RTLD_NOW);
    getInfo = (GetInfo)dlsym(mr, "MRMediaRemoteGetNowPlayingInfo");
    getPID = (GetPID)dlsym(mr, "MRMediaRemoteGetNowPlayingApplicationPID");
    getPlaying = (GetPlaying)dlsym(mr, "MRMediaRemoteGetNowPlayingApplicationIsPlaying");
    Register reg = (Register)dlsym(mr, "MRMediaRemoteRegisterForNowPlayingNotifications");
    if (!getInfo || !getPID || !getPlaying || !reg) {
        fprintf(stderr, "luch: MediaRemote is not there\n");
        exit(1);
    }
    reg(dispatch_get_main_queue());
    NSNotificationCenter *nc = [NSNotificationCenter defaultCenter];
    for (NSString *name in @[ @"kMRMediaRemoteNowPlayingInfoDidChangeNotification",
                              @"kMRMediaRemoteNowPlayingApplicationDidChangeNotification",
                              @"kMRMediaRemoteNowPlayingApplicationIsPlayingDidChangeNotification" ]) {
        [nc addObserverForName:name object:nil queue:nil usingBlock:^(NSNotification *n) {
            (void)n;
            emit();
        }];
    }
    // The sender ends us by closing our stdin; a crashed sender does too.
    dispatch_source_t eof = dispatch_source_create(DISPATCH_SOURCE_TYPE_READ, STDIN_FILENO, 0, dispatch_get_main_queue());
    dispatch_source_set_event_handler(eof, ^{
        char b[256];
        if (read(STDIN_FILENO, b, sizeof b) <= 0) exit(0);
    });
    dispatch_resume(eof);
    emit();
    // Some players move the position without a notification: look again now and then.
    dispatch_source_t tick = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, dispatch_get_main_queue());
    dispatch_source_set_timer(tick, dispatch_time(DISPATCH_TIME_NOW, 5 * NSEC_PER_SEC), 5 * NSEC_PER_SEC, NSEC_PER_SEC);
    dispatch_source_set_event_handler(tick, ^{ emit(); });
    dispatch_resume(tick);
    CFRunLoopRun();
}
