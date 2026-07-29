/* SPDX-License-Identifier: MIT */
#define _GNU_SOURCE

#include <assert.h>
#include <dirent.h>
#include <errno.h>
#include <fcntl.h>
#include <limits.h>
#include <linux/input.h>
#include <pthread.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <sys/stat.h>
#include <unistd.h>

#if defined(__GNUC__)
#define LIBINPUT_EXPORT __attribute__((visibility("default")))
#else
#define LIBINPUT_EXPORT
#endif

struct libinput;
struct libinput_device;
struct libinput_event;
struct libinput_event_keyboard;
struct libinput_event_pointer;
struct libinput_seat;

enum libinput_event_type {
    LIBINPUT_EVENT_NONE = 0,
    LIBINPUT_EVENT_DEVICE_ADDED = 1,
    LIBINPUT_EVENT_DEVICE_REMOVED = 2,
    LIBINPUT_EVENT_KEYBOARD_KEY = 300,
    LIBINPUT_EVENT_POINTER_MOTION = 400,
    LIBINPUT_EVENT_POINTER_MOTION_ABSOLUTE = 401,
    LIBINPUT_EVENT_POINTER_BUTTON = 402,
};

enum libinput_key_state {
    LIBINPUT_KEY_STATE_RELEASED = 0,
    LIBINPUT_KEY_STATE_PRESSED = 1,
};

enum libinput_button_state {
    LIBINPUT_BUTTON_STATE_RELEASED = 0,
    LIBINPUT_BUTTON_STATE_PRESSED = 1,
};

enum libinput_led {
    LIBINPUT_LED_NUM_LOCK = 1 << 0,
    LIBINPUT_LED_CAPS_LOCK = 1 << 1,
    LIBINPUT_LED_SCROLL_LOCK = 1 << 2,
    LIBINPUT_LED_COMPOSE = 1 << 3,
    LIBINPUT_LED_KANA = 1 << 4,
};

struct usage_count {
    struct libinput *context;
    struct libinput_seat *seat;
    uint32_t code;
    uint32_t count;
    struct usage_count *next;
};

static pthread_mutex_t state_lock = PTHREAD_MUTEX_INITIALIZER;
static struct usage_count *usage_counts;

static uint32_t
update_usage_count_locked(struct libinput *context,
                          struct libinput_seat *seat,
                          uint32_t code,
                          int pressed,
                          int *updated)
{
    struct usage_count **link = &usage_counts;
    struct usage_count *entry;

    while (*link) {
        entry = *link;
        if (entry->context == context && entry->seat == seat && entry->code == code)
            break;
        link = &entry->next;
    }

    entry = *link;
    if (pressed) {
        if (!entry) {
            entry = calloc(1, sizeof(*entry));
            if (!entry) {
                *updated = 0;
                return 0;
            }
            entry->context = context;
            entry->seat = seat;
            entry->code = code;
            entry->count = 1;
            entry->next = usage_counts;
            usage_counts = entry;
        } else if (entry->count != UINT32_MAX) {
            entry->count++;
        }
        *updated = 1;
        return entry->count;
    }

    if (!entry) {
        *updated = 1;
        return 0;
    }

    if (entry->count > 1) {
        entry->count--;
        *updated = 1;
        return entry->count;
    }

    *link = entry->next;
    free(entry);
    *updated = 1;
    return 0;
}

static void
clear_context_counts_locked(struct libinput *context)
{
    struct usage_count **link = &usage_counts;

    while (*link) {
        struct usage_count *entry = *link;
        if (entry->context == context) {
            *link = entry->next;
            free(entry);
        } else {
            link = &entry->next;
        }
    }
}

#ifdef LIBINPUT_RS_KEYBOARD_COMPAT_TEST

int
main(void)
{
    struct libinput *context = (struct libinput *)(uintptr_t)1;
    struct libinput_seat *seat = (struct libinput_seat *)(uintptr_t)2;
    int updated = 0;

    pthread_mutex_lock(&state_lock);

    assert(update_usage_count_locked(context, seat, 42, 1, &updated) == 1);
    assert(updated);
    assert(update_usage_count_locked(context, seat, 30, 1, &updated) == 1);
    assert(update_usage_count_locked(context, seat, 30, 0, &updated) == 0);
    assert(update_usage_count_locked(context, seat, 42, 0, &updated) == 0);

    assert(update_usage_count_locked(context, seat, 42, 1, &updated) == 1);
    assert(update_usage_count_locked(context, seat, 42, 1, &updated) == 2);
    assert(update_usage_count_locked(context, seat, 42, 0, &updated) == 1);
    assert(update_usage_count_locked(context, seat, 42, 0, &updated) == 0);
    assert(update_usage_count_locked(context, seat, 42, 0, &updated) == 0);

    clear_context_counts_locked(context);
    assert(usage_counts == NULL);

    pthread_mutex_unlock(&state_lock);
    return 0;
}

#else

enum cached_event_kind {
    CACHED_KEYBOARD_EVENT,
    CACHED_POINTER_EVENT,
};

struct event_count {
    struct libinput_event *event;
    struct libinput *context;
    enum cached_event_kind kind;
    uint32_t count;
    struct event_count *next;
};

static struct event_count *event_counts;

extern struct libinput_event *libinput_rs_get_event(struct libinput *context);
extern void libinput_rs_event_destroy(struct libinput_event *event);
extern struct libinput *libinput_rs_unref(struct libinput *context);
extern uint32_t libinput_rs_event_keyboard_get_seat_key_count(
    struct libinput_event_keyboard *event);
extern uint32_t libinput_rs_event_pointer_get_seat_button_count(
    struct libinput_event_pointer *event);

extern enum libinput_event_type libinput_event_get_type(struct libinput_event *event);
extern struct libinput_device *libinput_event_get_device(struct libinput_event *event);
extern struct libinput_seat *libinput_device_get_seat(struct libinput_device *device);
extern const char *libinput_device_get_devnode(struct libinput_device *device);
extern struct libinput_event_keyboard *libinput_event_get_keyboard_event(
    struct libinput_event *event);
extern uint32_t libinput_event_keyboard_get_key(struct libinput_event_keyboard *event);
extern enum libinput_key_state libinput_event_keyboard_get_key_state(
    struct libinput_event_keyboard *event);
extern struct libinput_event_pointer *libinput_event_get_pointer_event(
    struct libinput_event *event);
extern uint32_t libinput_event_pointer_get_button(struct libinput_event_pointer *event);
extern enum libinput_button_state libinput_event_pointer_get_button_state(
    struct libinput_event_pointer *event);

static void
cache_event_count_locked(struct libinput_event *event,
                         struct libinput *context,
                         enum cached_event_kind kind,
                         uint32_t count)
{
    struct event_count *entry = malloc(sizeof(*entry));
    if (!entry)
        return;

    entry->event = event;
    entry->context = context;
    entry->kind = kind;
    entry->count = count;
    entry->next = event_counts;
    event_counts = entry;
}

static int
get_cached_event_count_locked(struct libinput_event *event,
                              enum cached_event_kind kind,
                              uint32_t *count)
{
    struct event_count *entry;

    for (entry = event_counts; entry; entry = entry->next) {
        if (entry->event == event && entry->kind == kind) {
            *count = entry->count;
            return 1;
        }
    }

    return 0;
}

static void
remove_cached_event_locked(struct libinput_event *event)
{
    struct event_count **link = &event_counts;

    while (*link) {
        struct event_count *entry = *link;
        if (entry->event == event) {
            *link = entry->next;
            free(entry);
            return;
        }
        link = &entry->next;
    }
}

LIBINPUT_EXPORT
struct libinput_event *
libinput_get_event(struct libinput *context)
{
    struct libinput_event *event = libinput_rs_get_event(context);
    struct libinput_device *device;
    struct libinput_seat *seat;
    enum libinput_event_type type;
    uint32_t code;
    uint32_t count;
    int pressed;
    int updated;
    enum cached_event_kind kind;

    if (!event)
        return NULL;

    type = libinput_event_get_type(event);
    if (type == LIBINPUT_EVENT_KEYBOARD_KEY) {
        struct libinput_event_keyboard *keyboard =
            libinput_event_get_keyboard_event(event);
        if (!keyboard)
            return event;
        code = libinput_event_keyboard_get_key(keyboard);
        pressed = libinput_event_keyboard_get_key_state(keyboard) ==
                  LIBINPUT_KEY_STATE_PRESSED;
        kind = CACHED_KEYBOARD_EVENT;
    } else if (type == LIBINPUT_EVENT_POINTER_BUTTON) {
        struct libinput_event_pointer *pointer =
            libinput_event_get_pointer_event(event);
        if (!pointer)
            return event;
        code = libinput_event_pointer_get_button(pointer);
        pressed = libinput_event_pointer_get_button_state(pointer) ==
                  LIBINPUT_BUTTON_STATE_PRESSED;
        kind = CACHED_POINTER_EVENT;
    } else {
        return event;
    }

    device = libinput_event_get_device(event);
    if (!device)
        return event;
    seat = libinput_device_get_seat(device);
    if (!seat)
        return event;

    pthread_mutex_lock(&state_lock);
    count = update_usage_count_locked(context, seat, code, pressed, &updated);
    if (updated)
        cache_event_count_locked(event, context, kind, count);
    pthread_mutex_unlock(&state_lock);

    return event;
}

LIBINPUT_EXPORT
uint32_t
libinput_event_keyboard_get_seat_key_count(struct libinput_event_keyboard *event)
{
    uint32_t count;

    if (!event)
        return 0;

    pthread_mutex_lock(&state_lock);
    if (get_cached_event_count_locked((struct libinput_event *)event,
                                      CACHED_KEYBOARD_EVENT,
                                      &count)) {
        pthread_mutex_unlock(&state_lock);
        return count;
    }
    pthread_mutex_unlock(&state_lock);

    return libinput_rs_event_keyboard_get_seat_key_count(event);
}

LIBINPUT_EXPORT
uint32_t
libinput_event_pointer_get_seat_button_count(struct libinput_event_pointer *event)
{
    uint32_t count;

    if (!event)
        return 0;

    pthread_mutex_lock(&state_lock);
    if (get_cached_event_count_locked((struct libinput_event *)event,
                                      CACHED_POINTER_EVENT,
                                      &count)) {
        pthread_mutex_unlock(&state_lock);
        return count;
    }
    pthread_mutex_unlock(&state_lock);

    return libinput_rs_event_pointer_get_seat_button_count(event);
}

LIBINPUT_EXPORT
void
libinput_event_destroy(struct libinput_event *event)
{
    if (event) {
        pthread_mutex_lock(&state_lock);
        remove_cached_event_locked(event);
        pthread_mutex_unlock(&state_lock);
    }
    libinput_rs_event_destroy(event);
}

LIBINPUT_EXPORT
struct libinput *
libinput_unref(struct libinput *context)
{
    struct libinput *remaining = libinput_rs_unref(context);

    if (!remaining && context) {
        pthread_mutex_lock(&state_lock);
        clear_context_counts_locked(context);
        pthread_mutex_unlock(&state_lock);
    }

    return remaining;
}

static int
write_led_state(int fd, enum libinput_led leds)
{
    static const struct {
        enum libinput_led libinput;
        unsigned short evdev;
    } map[] = {
        { LIBINPUT_LED_NUM_LOCK, LED_NUML },
        { LIBINPUT_LED_CAPS_LOCK, LED_CAPSL },
        { LIBINPUT_LED_SCROLL_LOCK, LED_SCROLLL },
        { LIBINPUT_LED_COMPOSE, LED_COMPOSE },
        { LIBINPUT_LED_KANA, LED_KANA },
    };
    struct input_event events[sizeof(map) / sizeof(map[0]) + 1];
    ssize_t written;
    size_t i;

    memset(events, 0, sizeof(events));
    for (i = 0; i < sizeof(map) / sizeof(map[0]); i++) {
        events[i].type = EV_LED;
        events[i].code = map[i].evdev;
        events[i].value = !!(leds & map[i].libinput);
    }
    events[i].type = EV_SYN;
    events[i].code = SYN_REPORT;

    do {
        written = write(fd, events, sizeof(events));
    } while (written < 0 && errno == EINTR);

    return written == (ssize_t)sizeof(events);
}

LIBINPUT_EXPORT
void
libinput_device_led_update(struct libinput_device *device, enum libinput_led leds)
{
    const char *devnode;
    struct stat device_stat;
    DIR *directory;
    struct dirent *entry;

    if (!device)
        return;

    devnode = libinput_device_get_devnode(device);
    if (!devnode || stat(devnode, &device_stat) < 0 || !S_ISCHR(device_stat.st_mode))
        return;

    directory = opendir("/proc/self/fd");
    if (!directory)
        return;

    while ((entry = readdir(directory))) {
        char *end = NULL;
        long value;
        int fd;
        int flags;
        struct stat fd_stat;

        errno = 0;
        value = strtol(entry->d_name, &end, 10);
        if (errno || !end || *end != '\0' || value < 0 || value > INT_MAX)
            continue;

        fd = (int)value;
        if (fstat(fd, &fd_stat) < 0 || !S_ISCHR(fd_stat.st_mode) ||
            fd_stat.st_rdev != device_stat.st_rdev)
            continue;

        flags = fcntl(fd, F_GETFL);
        if (flags < 0 || (flags & O_ACCMODE) == O_RDONLY)
            continue;

        if (write_led_state(fd, leds))
            break;
    }

    closedir(directory);
}

#endif
