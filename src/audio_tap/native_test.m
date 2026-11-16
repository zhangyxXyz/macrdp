// No HAL calls, permission prompts or changes to the user's audio devices.
#include "native.m"
#include <assert.h>
#include <stdio.h>

int main(void) {
    AudioStreamBasicDescription format = {.mSampleRate=48000, .mFormatID=kAudioFormatLinearPCM,
        .mFormatFlags=kAudioFormatFlagIsFloat, .mBitsPerChannel=32, .mChannelsPerFrame=2, .mBytesPerFrame=8};
    assert(supported_format(format));
    format.mBytesPerFrame=16; assert(!supported_format(format));
    format.mBytesPerFrame=4; format.mFormatFlags |= kAudioFormatFlagIsNonInterleaved;
    assert(supported_format(format));
    format.mSampleRate=NAN; assert(!supported_format(format));
    format.mSampleRate=48000.5; assert(!supported_format(format));
    format.mSampleRate=48000; format.mBitsPerChannel=16; assert(!supported_format(format));
    struct Tap t = {0}; pthread_mutex_init(&t.mutex, NULL); atomic_init(&t.invalid, false);
    float l[CAPACITY], r[CAPACITY];
    float interleaved[] = {0.1, 0.2, 0.3, 0.4};
    AudioBufferList input = {.mNumberBuffers=1, .mBuffers={{2, sizeof(interleaved), interleaved}}};
    receive(0, NULL, &input, NULL, NULL, NULL, &t);
    assert(macrdp_tap_read(&t, l, r, CAPACITY) == 2);
    assert(l[0] == interleaved[0] && r[0] == interleaved[1]);
    assert(l[1] == interleaved[2] && r[1] == interleaved[3]);
    assert(macrdp_tap_read(&t, l, r, CAPACITY) == 0);
    struct { UInt32 count; AudioBuffer buffers[2]; } planar = {2, {{1, 8, interleaved}, {1, 8, interleaved+2}}};
    receive(0, NULL, (AudioBufferList *)&planar, NULL, NULL, NULL, &t);
    assert(macrdp_tap_read(&t, l, r, 1) == 1);
    assert(l[0] == interleaved[0] && r[0] == interleaved[2]);
    assert(macrdp_tap_read(&t, l, r, CAPACITY) == 1);
    // Realtime callback never waits for a slow reader.
    pthread_mutex_lock(&t.mutex);
    receive(0, NULL, &input, NULL, NULL, NULL, &t);
    pthread_mutex_unlock(&t.mutex);
    assert(t.count == 0);
    float *large = calloc(2 * (CAPACITY+7), sizeof(float));
    for (size_t i=0; i<CAPACITY+7; i++) { large[2*i] = i; large[2*i+1] = -((float)i); }
    input.mBuffers[0] = (AudioBuffer){2, 2*(CAPACITY+7)*sizeof(float), large};
    receive(0, NULL, &input, NULL, NULL, NULL, &t);
    assert(macrdp_tap_read(&t, l, r, CAPACITY) == CAPACITY);
    assert(l[0] == 7 && r[CAPACITY-1] == -(CAPACITY+6));
    free(large);
    input.mBuffers[0].mNumberChannels = 0;
    receive(0, NULL, &input, NULL, NULL, NULL, &t);
    assert(macrdp_tap_read(&t, l, r, CAPACITY) == -1);
    pthread_mutex_destroy(&t.mutex);
    puts("native audio ring: interleaved, planar, partial read, empty, contention, overflow, invalid layout passed");
}
