// obsbot-uvc: the OBSBOT webcam's standard USB video (UVC) camera controls, straight from this Mac, with no
// OBSBOT Center (docs/obsbot.md, "Direct USB"). eve builds it with clang on first use (obsbot_usb.py).
//
// The Mac's UVC driver owns the camera, so libusb is refused ("access denied"). IOKit's DeviceRequest still sends
// a control request to the device's default pipe, and the camera answers it (measured 2026-10-11 on a Tiny 3).
// Requests go to the camera terminal (unit 1) of the video control interface (0), UVC 1.5 section 4.2.2.1.
//
//   obsbot-uvc get                 {"ok":true,"pan":<deg>,"tilt":<deg>,"zoom":<0-100>,"pan_min":..,"pan_max":..,
//                                   "tilt_min":..,"tilt_max":..,"zoom_min":..,"zoom_max":..}
//   obsbot-uvc pantilt PAN TILT    absolute, in degrees (+ pan as the camera reports it, + tilt is up)
//   obsbot-uvc turn PAN TILT [SPEED]  continuous: each -1, 0 or 1, at SPEED (the camera's max when left out or
//                                  higher); "turn 0 0" stops
//   obsbot-uvc zoom N              absolute zoom, in the camera's units (0-100 on a Tiny 3)
//
// Every answer is one line of JSON with "ok". A camera that isn't plugged in is {"ok":false,"error":"no camera"}.
#include <CoreFoundation/CoreFoundation.h>
#include <IOKit/IOCFPlugIn.h>
#include <IOKit/IOKitLib.h>
#include <IOKit/usb/IOUSBLib.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>

#define VENDOR 0x3564      // OBSBOT
#define TERMINAL 0x0100    // wIndex: camera terminal 1 on interface 0
#define CT_ZOOM_ABSOLUTE 0x0B
#define CT_PANTILT_ABSOLUTE 0x0D
#define CT_PANTILT_RELATIVE 0x0E
#define SET_CUR 0x01
#define GET_CUR 0x81
#define GET_MIN 0x82
#define GET_MAX 0x83
#define ARCSEC 3600.0      // UVC pan/tilt units are 1/3600 of a degree

static IOUSBDeviceInterface **dev;
static char product[64] = "OBSBOT camera";

static int request(UInt8 type, UInt8 req, UInt8 selector, void *buf, UInt16 len) {
    IOUSBDevRequestTO q = {type, req, (UInt16)(selector << 8), TERMINAL, len, buf, 0, 1000, 1000};
    return (*dev)->DeviceRequestTO(dev, &q) == kIOReturnSuccess && q.wLenDone == len;
}

static int get(UInt8 req, UInt8 selector, void *buf, UInt16 len) { return request(0xA1, req, selector, buf, len); }
static int set(UInt8 selector, void *buf, UInt16 len) { return request(0x21, SET_CUR, selector, buf, len); }

static int open_camera(void) {
    io_iterator_t it;
    // every USB device, filtered by its vendor here: IOKit's USB matching ignores a vendor id with no product id
    if (IOServiceGetMatchingServices(kIOMainPortDefault, IOServiceMatching(kIOUSBDeviceClassName), &it) !=
        KERN_SUCCESS)
        return 0;
    io_service_t svc;
    while ((svc = IOIteratorNext(it))) {   // the first OBSBOT device that answers a pan/tilt read
        CFTypeRef vref = IORegistryEntryCreateCFProperty(svc, CFSTR(kUSBVendorID), NULL, 0);
        int vendor = 0;
        if (vref) {
            CFNumberGetValue((CFNumberRef)vref, kCFNumberIntType, &vendor);
            CFRelease(vref);
        }
        if (vendor != VENDOR) {
            IOObjectRelease(svc);
            continue;
        }
        IOCFPlugInInterface **plug = NULL;
        SInt32 score;
        if (IOCreatePlugInInterfaceForService(svc, kIOUSBDeviceUserClientTypeID, kIOCFPlugInInterfaceID, &plug,
                                              &score) == KERN_SUCCESS && plug) {
            (*plug)->QueryInterface(plug, CFUUIDGetUUIDBytes(kIOUSBDeviceInterfaceID), (LPVOID *)&dev);
            (*plug)->Release(plug);
            SInt32 pt[2];
            if (dev && get(GET_CUR, CT_PANTILT_ABSOLUTE, pt, 8)) {
                CFTypeRef name = IORegistryEntryCreateCFProperty(svc, CFSTR(kUSBProductString), NULL, 0);
                if (name && CFGetTypeID(name) == CFStringGetTypeID())
                    CFStringGetCString((CFStringRef)name, product, sizeof product, kCFStringEncodingUTF8);
                if (name) CFRelease(name);
                for (char *c = product; *c; c++)
                    if (*c == '"' || *c == '\\' || (unsigned char)*c < 0x20) *c = ' ';
                IOObjectRelease(svc);
                IOObjectRelease(it);
                return 1;
            }
            if (dev) (*dev)->Release(dev);
            dev = NULL;
        }
        IOObjectRelease(svc);
    }
    IOObjectRelease(it);
    return 0;
}

static int fail(const char *why) {
    printf("{\"ok\":false,\"error\":\"%s\"}\n", why);
    return 1;
}

int main(int argc, char **argv) {
    if (argc < 2) return fail("usage: obsbot-uvc get | pantilt PAN TILT | turn PAN TILT [SPEED] | zoom N");
    if (!open_camera()) return fail("no camera");
    const char *cmd = argv[1];
    if (strcmp(cmd, "get") == 0) {
        SInt32 pt[2], mn[2], mx[2];
        UInt16 z = 0, zmin = 0, zmax = 0;
        if (!get(GET_CUR, CT_PANTILT_ABSOLUTE, pt, 8) || !get(GET_MIN, CT_PANTILT_ABSOLUTE, mn, 8) ||
            !get(GET_MAX, CT_PANTILT_ABSOLUTE, mx, 8))
            return fail("pan/tilt unreadable");
        int has_zoom = get(GET_CUR, CT_ZOOM_ABSOLUTE, &z, 2) && get(GET_MIN, CT_ZOOM_ABSOLUTE, &zmin, 2) &&
                       get(GET_MAX, CT_ZOOM_ABSOLUTE, &zmax, 2);
        printf("{\"ok\":true,\"name\":\"%s\",\"pan\":%.2f,\"tilt\":%.2f,\"pan_min\":%.1f,\"pan_max\":%.1f,\"tilt_min\":%.1f,"
               "\"tilt_max\":%.1f",
               product, pt[0] / ARCSEC, pt[1] / ARCSEC, mn[0] / ARCSEC, mx[0] / ARCSEC, mn[1] / ARCSEC, mx[1] / ARCSEC);
        if (has_zoom) printf(",\"zoom\":%u,\"zoom_min\":%u,\"zoom_max\":%u", z, zmin, zmax);
        printf("}\n");
        return 0;
    }
    if (strcmp(cmd, "pantilt") == 0 && argc == 4) {
        SInt32 pt[2] = {(SInt32)(atof(argv[2]) * ARCSEC), (SInt32)(atof(argv[3]) * ARCSEC)};
        if (!set(CT_PANTILT_ABSOLUTE, pt, 8)) return fail("pan/tilt refused");
        printf("{\"ok\":true}\n");
        return 0;
    }
    if (strcmp(cmd, "turn") == 0 && (argc == 4 || argc == 5)) {
        // bPanRelative, bPanSpeed, bTiltRelative, bTiltSpeed (UVC 1.5 table 4-19); the speed is the camera's max
        SInt8 cur[4] = {0}, max[4] = {0};
        get(GET_MAX, CT_PANTILT_RELATIVE, max, 4);
        int p = atoi(argv[2]), t = atoi(argv[3]);
        cur[0] = (SInt8)(p > 0 ? 1 : p < 0 ? -1 : 0);
        int want = argc == 5 ? atoi(argv[4]) : 0;
        int pan_speed = max[1] ? max[1] : 1, tilt_speed = max[3] ? max[3] : 1;
        if (want > 0 && want < pan_speed) pan_speed = want;
        if (want > 0 && want < tilt_speed) tilt_speed = want;
        cur[1] = (SInt8)(p ? pan_speed : 0);
        cur[2] = (SInt8)(t > 0 ? 1 : t < 0 ? -1 : 0);
        cur[3] = (SInt8)(t ? tilt_speed : 0);
        if (!set(CT_PANTILT_RELATIVE, cur, 4)) return fail("turn refused");
        printf("{\"ok\":true,\"speed\":[%d,%d]}\n", cur[1], cur[3]);
        return 0;
    }
    if (strcmp(cmd, "zoom") == 0 && argc == 3) {
        UInt16 z = (UInt16)atoi(argv[2]);
        if (!set(CT_ZOOM_ABSOLUTE, &z, 2)) return fail("zoom refused");
        printf("{\"ok\":true}\n");
        return 0;
    }
    return fail("usage: obsbot-uvc get | pantilt PAN TILT | turn PAN TILT [SPEED] | zoom N");
}
