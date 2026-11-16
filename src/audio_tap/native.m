// Core Audio process-tap boundary. No system default-device, volume or mute writes.
// The private tap mutes its sources only while its private aggregate is read.
#import <Foundation/Foundation.h>
#import <CoreAudio/CoreAudio.h>
#import <CoreAudio/AudioHardwareTapping.h>
#import <CoreAudio/CATapDescription.h>
#include <pthread.h>
#include <stdatomic.h>
#include <math.h>

#define CAPACITY 8192
struct Tap {
    AudioObjectID tap, device;
    AudioDeviceIOProcID proc;
    pthread_mutex_t mutex;
    float left[CAPACITY], right[CAPACITY];
    size_t head, count;
    double rate;
    _Atomic bool invalid;
};

// Neither allocates nor waits on the realtime callback. If the reader holds
// the ring, drop this callback rather than delaying coreaudiod.
static OSStatus receive(AudioObjectID device, const AudioTimeStamp *now,
                        const AudioBufferList *input, const AudioTimeStamp *at,
                        AudioBufferList *output, const AudioTimeStamp *outAt, void *context) {
    (void)device; (void)now; (void)at; (void)output; (void)outAt;
    struct Tap *t = context;
    if (!input || !input->mNumberBuffers) return noErr;
    const AudioBuffer *a = &input->mBuffers[0];
    const AudioBuffer *b = input->mNumberBuffers == 2 ? &input->mBuffers[1] : a;
    bool planar = input->mNumberBuffers == 2 && a->mNumberChannels == 1 && b->mNumberChannels == 1;
    bool interleaved = input->mNumberBuffers == 1 && a->mNumberChannels == 2;
    if ((!planar && !interleaved) || a->mDataByteSize % (sizeof(float) * a->mNumberChannels) ||
        (planar && b->mDataByteSize != a->mDataByteSize)) {
        atomic_store(&t->invalid, true); return noErr;
    }
    if (!a->mData || !b->mData) return noErr;
    size_t frames = a->mDataByteSize / (sizeof(float) * a->mNumberChannels);
    if (pthread_mutex_trylock(&t->mutex)) return noErr;
    const float *l = a->mData, *r = b->mData;
    // Retain freshest audio, with bounded memory even after a stalled reader.
    size_t start = frames > CAPACITY ? frames - CAPACITY : 0;
    for (size_t i = start; i < frames; i++) {
        if (t->count == CAPACITY) { t->head = (t->head + 1) % CAPACITY; t->count--; }
        size_t dst = (t->head + t->count++) % CAPACITY;
        t->left[dst] = l[planar ? i : 2*i];
        t->right[dst] = r[planar ? i : 2*i+1];
    }
    pthread_mutex_unlock(&t->mutex);
    return noErr;
}

static bool supported_format(AudioStreamBasicDescription f) {
    return f.mFormatID == kAudioFormatLinearPCM &&
        (f.mFormatFlags & kAudioFormatFlagIsFloat) &&
        !(f.mFormatFlags & kAudioFormatFlagIsBigEndian) &&
        f.mBitsPerChannel == 32 && f.mChannelsPerFrame == 2 &&
        f.mBytesPerFrame == ((f.mFormatFlags & kAudioFormatFlagIsNonInterleaved) ? 4 : 8) &&
        isfinite(f.mSampleRate) && f.mSampleRate >= 8000 && f.mSampleRate <= 192000 &&
        floor(f.mSampleRate) == f.mSampleRate;
}

static OSStatus close_tap(struct Tap *t) {
    if (!t) return noErr;
    OSStatus failure = noErr;
    bool callback_removed = true;
    if (t->proc) {
        AudioDeviceStop(t->device, t->proc);
        OSStatus status = AudioDeviceDestroyIOProcID(t->device, t->proc);
        // Still attempt aggregate/tap teardown to release source muting even
        // when HAL rejects IOProc destruction. Retain context in that rare
        // failure case; freeing a possibly live callback would be unsafe.
        if (status) { failure = status; callback_removed = false; }
    }
    if (t->device) { OSStatus status = AudioHardwareDestroyAggregateDevice(t->device); if (status) failure = status; }
    if (@available(macOS 14.2, *)) {
        if (t->tap) {
            OSStatus status = AudioHardwareDestroyProcessTap(t->tap);
            if (status) failure = status;
        }
    }
    if (callback_removed) { pthread_mutex_destroy(&t->mutex); free(t); }
    return failure;
}

void *macrdp_tap_open(double *rate, int32_t *error) {
    *error = kAudioHardwareUnsupportedOperationError;
    if (@available(macOS 14.2, *)) {
        @autoreleasepool {
            struct Tap *t = calloc(1, sizeof(*t));
            if (!t) { *error = -108; return NULL; }
            pthread_mutex_init(&t->mutex, NULL);
            atomic_init(&t->invalid, false);
            CATapDescription *description = [[CATapDescription alloc] initStereoGlobalTapButExcludeProcesses:@[]];
            description.name = @"macrdp remote audio";
            description.privateTap = YES;
            description.muteBehavior = CATapMutedWhenTapped;
            OSStatus status = AudioHardwareCreateProcessTap(description, &t->tap);
            AudioStreamBasicDescription format = {0};
            AudioObjectPropertyAddress address = {kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, 0};
            UInt32 size = sizeof(format);
            if (!status) status = AudioObjectGetPropertyData(t->tap, &address, 0, NULL, &size, &format);
            if (!status && !supported_format(format))
                status = kAudioDeviceUnsupportedFormatError;
            if (!status) {
                t->rate = format.mSampleRate;
                NSDictionary *aggregate = @{
                    @kAudioAggregateDeviceNameKey: @"macrdp private audio capture",
                    @kAudioAggregateDeviceUIDKey: NSUUID.UUID.UUIDString,
                    @kAudioAggregateDeviceIsPrivateKey: @YES,
                    @kAudioAggregateDeviceTapAutoStartKey: @NO,
                    @kAudioAggregateDeviceTapListKey: @[@{
                        @kAudioSubTapUIDKey: description.UUID.UUIDString,
                        @kAudioSubTapDriftCompensationKey: @YES
                    }]
                };
                status = AudioHardwareCreateAggregateDevice((__bridge CFDictionaryRef)aggregate, &t->device);
            }
            if (!status) status = AudioDeviceCreateIOProcID(t->device, receive, t, &t->proc);
            if (!status) status = AudioDeviceStart(t->device, t->proc);
            if (status) { OSStatus cleanup = close_tap(t); if (cleanup) NSLog(@"macrdp audio tap initialization cleanup failed: %d", cleanup); *error = status; return NULL; }
            *rate = t->rate; *error = 0;
            return t;
        }
    }
    return NULL;
}

// Called by the capture worker, never on the RDP/network thread.
int32_t macrdp_tap_read(void *handle, float *left, float *right, uint32_t capacity) {
    struct Tap *t = handle;
    if (atomic_load(&t->invalid)) return -1;
    pthread_mutex_lock(&t->mutex);
    size_t n = MIN(t->count, capacity);
    for (size_t i = 0; i < n; i++) {
        size_t index = (t->head + i) % CAPACITY;
        left[i] = t->left[index]; right[i] = t->right[index];
    }
    t->head = (t->head + n) % CAPACITY; t->count -= n;
    pthread_mutex_unlock(&t->mutex);
    return (int32_t)n;
}

// A format change must rebuild the resampler; never decode a new rate/layout
// using the old ASBD. Called at low frequency outside the realtime callback.
bool macrdp_tap_format_unchanged(void *handle) {
    struct Tap *t = handle;
    AudioStreamBasicDescription format = {0};
    AudioObjectPropertyAddress address = {kAudioTapPropertyFormat, kAudioObjectPropertyScopeGlobal, 0};
    UInt32 size = sizeof(format);
    return AudioObjectGetPropertyData(t->tap, &address, 0, NULL, &size, &format) == noErr &&
        supported_format(format) && format.mSampleRate == t->rate;
}
int32_t macrdp_tap_close(void *handle) { return close_tap(handle); }
