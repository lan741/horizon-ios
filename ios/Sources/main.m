// Horizon for iOS: the app's `main`. The static library beside it (target/aarch64-apple-ios)
// runs the game in this process; winit's iOS backend owns `UIApplicationMain` from the call
// below, so nothing here may create a `UIWindow` first.
#import <Foundation/Foundation.h>

#include <stdint.h>

/// Mirrors `horizon_ios::Paths` (`crates/horizon-ios/src/lib.rs`): NUL-terminated UTF-8 paths,
/// or NULL for "not supplied".
typedef struct {
    const char *documents;
    const char *support;
    const char *resources;
} HorizonPaths;

extern int32_t horizon_ios_run(const HorizonPaths *paths);

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
        // Resources shipped inside the app (the CI build's option). Absent: the game looks in
        // `<Documents>/horizon-resources`, where the Files app can put them.
        NSString *bundled = [[NSBundle mainBundle] pathForResource:@"horizon-resources" ofType:nil];
        HorizonPaths paths = {
            gameFolder.UTF8String,
            supportFolder.UTF8String,
            bundled.length > 0 ? bundled.UTF8String : NULL,
        };
        return horizon_ios_run(&paths);
    }
}