cmake_minimum_required(VERSION 3.21)
project(traytray_kde_plugin_spike LANGUAGES CXX)

# Plain CMake on purpose: extra-cmake-modules is not assumed to be present.
set(CMAKE_CXX_STANDARD 17)
set(CMAKE_CXX_STANDARD_REQUIRED ON)
set(CMAKE_AUTOMOC ON)

find_package(Qt6 6.5 REQUIRED COMPONENTS Core Network Qml Quick)
qt_standard_project_setup(REQUIRES 6.5)

# The backing target is the plugin itself, so the module is one
# self-contained .so that can be copied anywhere without a second library on
# the loader path.
qt_add_library(traytrayspike MODULE)

qt_add_qml_module(traytrayspike
    URI org.traytray.spike
    VERSION 1.0
    PLUGIN_TARGET traytrayspike
    OUTPUT_DIRECTORY ${CMAKE_BINARY_DIR}/qml/org/traytray/spike
    SOURCES
        trayclient.h
        trayclient.cpp
)

target_link_libraries(traytrayspike PRIVATE Qt6::Core Qt6::Network Qt6::Qml)
