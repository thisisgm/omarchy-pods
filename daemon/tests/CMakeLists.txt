# OpenPods unit tests. Pulled in from parent CMakeLists only when
# BUILD_TESTING=ON (cmake default).
#
# Tests target headers/sources directly — no link against the OpenPods
# binary itself, since that would pull in BlueZ/PulseAudio/Qt6::Widgets.
# Each test file is a single tst_*.cpp with QTEST_GUILESS_MAIN.

find_package(Qt6 REQUIRED COMPONENTS Test Core Bluetooth)

# Centralized include path so tests can `#include "../eardetection.hpp"`
# without each adding their own target_include_directories.
function(openpods_add_test NAME)
    qt_add_executable(${NAME} ${ARGN})
    target_link_libraries(${NAME} PRIVATE Qt6::Test Qt6::Core)
    target_include_directories(${NAME} PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/..)
    set_target_properties(${NAME} PROPERTIES AUTOMOC ON)
    add_test(NAME ${NAME} COMMAND ${NAME})
endfunction()

# Headers must be listed so AUTOMOC scans Q_OBJECT classes defined in
# them — header-only QObject subclasses won't link otherwise.
openpods_add_test(tst_eardetection
    tst_eardetection.cpp
    ../eardetection.hpp
)

openpods_add_test(tst_autostartmanager
    tst_autostartmanager.cpp
    ../autostartmanager.hpp
)

openpods_add_test(tst_capturematch
    tst_capturematch.cpp
    ../media/capturematch.hpp
)

openpods_add_test(tst_profilechoice
    tst_profilechoice.cpp
    ../media/profilechoice.hpp
)

openpods_add_test(tst_logger
    tst_logger.cpp
    ../logger.h
)

openpods_add_test(tst_enums
    tst_enums.cpp
    ../enums.h
)

openpods_add_test(tst_basiccontrolcommand
    tst_basiccontrolcommand.cpp
    ../BasicControlCommand.hpp
)

openpods_add_test(tst_deviceinfo
    tst_deviceinfo.cpp
    ../deviceinfo.hpp
    ../battery.hpp
    ../eardetection.hpp
    ../enums.h
)

openpods_add_test(tst_battery
    tst_battery.cpp
    ../battery.hpp
)

openpods_add_test(tst_lowbatterywatcher
    tst_lowbatterywatcher.cpp
    ../lowbatterywatcher.hpp
)

openpods_add_test(tst_controlreconnect
    tst_controlreconnect.cpp
    ../controlreconnect.hpp
)

openpods_add_test(tst_scanduty
    tst_scanduty.cpp
    ../scanduty.hpp
)

openpods_add_test(tst_snaptogrid
    tst_snaptogrid.cpp
    ../snaptogrid.hpp
)

openpods_add_test(tst_ipcverb
    tst_ipcverb.cpp
    ../ipcverb.hpp
)

openpods_add_test(tst_ipcpath
    tst_ipcpath.cpp
    ../ipcpath.hpp
)

openpods_add_test(tst_notifier
    tst_notifier.cpp
    ../notifier.hpp
)

# The exception to the no-binary rule above: librepods-ctl links none of BlueZ, PulseAudio or Widgets.
openpods_add_test(tst_ctl
    tst_ctl.cpp
    ../ipcpath.hpp
)
find_package(Qt6 REQUIRED COMPONENTS Network)
target_link_libraries(tst_ctl PRIVATE Qt6::Network)
target_compile_definitions(tst_ctl PRIVATE
    OPENPODS_CTL_BINARY="$<TARGET_FILE:librepods-ctl>"
)
add_dependencies(tst_ctl librepods-ctl)

# Stochastic stress over protocol parsers — pulls in ble/bleutils
# which itself wants OpenSSL EVP for AES round-trips. Skipping AUTOMOC
# headers we don't need; bleutils.h is a free function header.
find_package(OpenSSL REQUIRED)
qt_add_executable(tst_stress
    tst_stress.cpp
    ../BasicControlCommand.hpp
    ../ble/bleutils.cpp
    ../ble/bleutils.h
)
target_link_libraries(tst_stress
    PRIVATE Qt6::Test Qt6::Core OpenSSL::SSL OpenSSL::Crypto
)
target_include_directories(tst_stress PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/..)
set_target_properties(tst_stress PROPERTIES AUTOMOC ON)
add_test(NAME tst_stress COMMAND tst_stress)
# tst_enums verifies icon filenames against on-disk assets/ — pass the
# path via compile def so the test doesn't have to assume cwd.
target_compile_definitions(tst_enums PRIVATE
    OPENPODS_ASSETS_DIR="${CMAKE_CURRENT_SOURCE_DIR}/../assets"
)

# QRCodeImageProvider pulls in QtQuick (image provider base) + QtGui
# (QImage/QPainter) + the vendored qrcodegen.cpp source.
find_package(Qt6 REQUIRED COMPONENTS Quick Gui)
qt_add_executable(tst_qrcodeimageprovider
    tst_qrcodeimageprovider.cpp
    ../QRCodeImageProvider.hpp
    ../thirdparty/QR-Code-generator/qrcodegen.cpp
)
target_link_libraries(tst_qrcodeimageprovider
    PRIVATE Qt6::Test Qt6::Core Qt6::Gui Qt6::Quick
)
target_include_directories(tst_qrcodeimageprovider PRIVATE ${CMAKE_CURRENT_SOURCE_DIR}/..)
set_target_properties(tst_qrcodeimageprovider PROPERTIES AUTOMOC ON)
add_test(NAME tst_qrcodeimageprovider COMMAND tst_qrcodeimageprovider -platform offscreen)

openpods_add_test(tst_blemanager
    tst_blemanager.cpp
    ../ble/blemanager.cpp
    ../ble/blemanager.h
)
# Only test that parses a QBluetoothDeviceInfo, so only one needing the module.
target_link_libraries(tst_blemanager PRIVATE Qt6::Bluetooth)
