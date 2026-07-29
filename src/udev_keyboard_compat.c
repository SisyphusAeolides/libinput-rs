/* SPDX-License-Identifier: MIT */
#define _GNU_SOURCE

#include <dlfcn.h>
#include <pthread.h>
#include <stddef.h>
#include <string.h>

struct udev_device;

typedef const char *(*udev_get_property_value_fn)(struct udev_device *device,
                                                   const char *key);

static pthread_once_t property_once = PTHREAD_ONCE_INIT;
static udev_get_property_value_fn real_get_property_value;

static void
resolve_get_property_value(void)
{
    real_get_property_value =
        (udev_get_property_value_fn)dlsym(RTLD_NEXT,
                                          "udev_device_get_property_value");
}

const char *
udev_device_get_property_value(struct udev_device *device, const char *key)
{
    const char *value;
    const char *key_value;

    if (!key)
        return NULL;

    pthread_once(&property_once, resolve_get_property_value);
    if (!real_get_property_value)
        return NULL;

    value = real_get_property_value(device, key);
    if (strcmp(key, "ID_INPUT_KEYBOARD") != 0 ||
        (value && strcmp(value, "1") == 0))
        return value;

    key_value = real_get_property_value(device, "ID_INPUT_KEY");
    if (key_value && strcmp(key_value, "1") == 0)
        return "1";

    return value;
}
