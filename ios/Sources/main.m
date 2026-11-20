// Horizon for iOS: the app's `main`.
//
// Two things happen here before Rust runs. First, a launch log is opened in the app's
// Documents folder and standard error is pointed at it: a Rust panic, a winit notice and
// the engine's own `eprintln!` lines all land where the Files app can read them. Second,
// the folders UIKit hands the app are computed once and passed to the static library,
// which runs the game in this process (winit's iOS backend owns `UIApplicationMain` from
// that call, so nothing here may create a `UIWindow` first).
//
// If the app disappears before the loading screen, `Documents/horizon-launch.log` says how
// far it got: `main entered`, then each step the library records.
#import <Foundation/Foundation.h>

#include <fcntl.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <unistd.h>

/// Mirrors `horizon_ios::Paths` (`crates/horizon-ios/src/lib.rs`): NUL-terminated UTF-8
/// paths, or NULL for "not supplied".
typedef struct {
    const char *documents;
    const char *support;
    const char *resources;
} HorizonPaths;

extern int32_t horizon_ios_run(const HorizonPaths *paths);

/// Opens the launch log and sends the process's standard error there.
static void open_launch_log(NSString *documents) {
    NSString *path = [documents stringByAppendingPathComponent:@"horizon-launch.log"];
    int fd = open(path.UTF8String, O_WRONLY | O_CREAT | O_APPEND, 0644);
    if (fd < 0) {
        return;
    }
    dup2(fd, STDERR_FILENO);
    setvbuf(stderr, NULL, _IONBF, 0);
    // The library appends its own lines by path; the environment carries it over.
    setenv("HORIZON_IOS_LOG", path.UTF8String, 1);
    if (fd != STDERR_FILENO) {
        close(fd);
    }
}

int main(int argc, char *argv[]) {
    (void)argc;
    (void)argv;
    @autoreleasepool {
        NSArray<NSString *> *documents =
            NSSearchPathForDirectoriesInDomains(NSDocumentDirectory, NSUserDomainMask, YES);
        NSArray<NSString *> *support =
            NSSearchPathForDirectoriesInDomains(NSApplicationSupportDirectory, NSUserDomainMask, YES);
        NSString *gameFolder = documents.firstObject ?: NSHomeDirectory();
        NSString *supportFolder = support.firstObject ?: gameFolder;
        open_launch_log(gameFolder);

        fprintf(stderr, "\n=== Horizon iOS launch %s ===\n", [[NSDate date] description].UTF8String);
        fprintf(stderr, "main entered\n");
        fprintf(stderr, "documents: %s\n", gameFolder.UTF8String);
        fprintf(stderr, "support:   %s\n", supportFolder.UTF8String);
        fprintf(stderr, "bundle:    %s\n", [[[NSBundle mainBundle] bundlePath] UTF8String]);
        fflush(stderr);

        // Resources shipped inside the app (the CI build's option). Absent: the game looks in
        // `<Documents>/horizon-resources`, where the Files app can put them.
        NSString *bundled = [[NSBundle mainBundle] pathForResource:@"horizon-resources" ofType:nil];
        fprintf(stderr, "resources: %s\n", bundled.length > 0 ? bundled.UTF8String : "(Documents/horizon-resources)");
        fflush(stderr);

        HorizonPaths paths = {
            gameFolder.UTF8String,
            supportFolder.UTF8String,
            bundled.length > 0 ? bundled.UTF8String : NULL,
        };
        fprintf(stderr, "calling horizon_ios_run\n");
        fflush(stderr);
        int32_t code = horizon_ios_run(&paths);
        fprintf(stderr, "horizon_ios_run returned %d\n", (int)code);
        fflush(stderr);
        return code;
    }
}