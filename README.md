# uff: untitled fuzzel frontend

![untitled fuzzel frontend](demo.png)

uff is strongly inspired by [raffi](https://github.com/chmouel/raffi/).

## feature comparison
|                            | **uff** | **raffi** |
| :------------------------: | :-----: | :-------: |
|         **icons**          | **yes** |  **yes**  |
|        **submenus**        | **yes** |    no     |
|    **custom icon dirs**    | **yes** |    no     |
|  **per-menu fuzzel args**  | **yes** |    no     |
| **per-menu fuzzel config** | **yes** |    no     |
|     **inline scripts**     |   no    |  **yes**  |
|   **conditional items**    |   no    |  **yes**  |

## configuration
```kdl
fuzzel-args foo bar baz

fuzzel-config {
    key value
}
// ^ inherited by submenus

icon-dir "/etc/whatever"
// ^ can be repeated for more dirs, inherited by submenus
// ^ also searches in XDG_DATA_DIRS by default

program "display name" {
    command foo bar baz
    // ^ required
    icon name
    // ^ will search the icon dirs for name.png or name.svg
    // ^ can also be a full path to the icon
}

menu "nested submenu" {
    icon name
    // submenus can contain all of the above items, plus an optional icon
}
```
