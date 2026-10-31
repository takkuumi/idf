#!/usr/bin/env python3
"""Exercise unmodified ESP-IDF NVS storage on an instrumented 24KB RAM NOR.

Only OS headers/logging and the partition hardware are substituted. The NVS
page, GC, blob versioning and CRC implementations come from the selected IDF.
Operation counts are emulated partition calls, not device timings. No device
or SDK source file is written.
"""

import argparse
import os
from pathlib import Path
import subprocess
import tempfile

ROOT = Path(__file__).resolve().parents[2]

HARNESS = r'''
#include <array>
#include <vector>
#include <cstdio>
#include <stdexcept>
#include <string>
#include <memory>
#include "nvs_storage.hpp"
#include "spi_flash_mmap.h"

static void require(bool condition, const char* text) {
    if (!condition) throw std::runtime_error(text);
}
static void ok(esp_err_t error) {
    if (error != ESP_OK) throw std::runtime_error("IDF error=" + std::to_string(error));
}
struct Nor : nvs::Partition {
    std::vector<uint8_t> bytes = std::vector<uint8_t>(0x6000, 0xff);
    size_t writes=0, erases=0, operations=0;
    long cut_after=-1;
    const char* get_partition_name() override { return "nvs"; }
    uint32_t get_address() override { return 0x9000; }
    uint32_t get_size() override { return bytes.size(); }
    bool get_readonly() override { return false; }
    esp_err_t read_raw(size_t at, void* out, size_t count) override { return read(at,out,count); }
    esp_err_t read(size_t at, void* out, size_t count) override {
        if (at > bytes.size() || count > bytes.size()-at) return ESP_ERR_INVALID_SIZE;
        std::memcpy(out,bytes.data()+at,count); return ESP_OK;
    }
    bool cut() { return cut_after >= 0 && operations++ >= size_t(cut_after); }
    esp_err_t write_raw(size_t at, const void* in, size_t count) override { return write(at,in,count); }
    esp_err_t write(size_t at, const void* in, size_t count) override {
        if (at > bytes.size() || count > bytes.size()-at) return ESP_ERR_INVALID_SIZE;
        if (cut()) return ESP_ERR_FLASH_OP_FAIL;
        writes++;
        auto src=static_cast<const uint8_t*>(in);
        for(size_t i=0;i<count;i++) bytes[at+i] &= src[i];
        return ESP_OK;
    }
    esp_err_t erase_range(size_t at, size_t count) override {
        if (at%4096 || count%4096 || at>bytes.size() || count>bytes.size()-at) return ESP_ERR_INVALID_SIZE;
        if (cut()) return ESP_ERR_FLASH_OP_FAIL;
        erases+=count/4096; std::memset(bytes.data()+at,0xff,count); return ESP_OK;
    }
    void counters() { writes=erases=operations=0; cut_after=-1; }
};
struct Store {
    nvs::Storage storage;
    uint8_t ns=0;
    Store(Nor& nor):storage(&nor) {
        ok(storage.init(0,6)); ok(storage.createOrOpenNamespace("gateway",true,ns));
    }
    esp_err_t put(const char* key,const std::vector<uint8_t>& bytes,bool erase_first=false) {
        if(erase_first) { auto e=storage.eraseItem(ns,key); if(e!=ESP_OK && e!=ESP_ERR_NVS_NOT_FOUND) return e; }
        return storage.writeItem(ns,nvs::ItemType::BLOB,key,bytes.data(),bytes.size());
    }
    std::vector<uint8_t> get(const char* key,size_t size) {
        std::vector<uint8_t> out(size); ok(storage.readItem(ns,nvs::ItemType::BLOB,key,out.data(),size)); return out;
    }
};
static std::vector<uint8_t> data(size_t size,uint8_t v) { return std::vector<uint8_t>(size,v); }
static void populate(Store& s) {
    ok(s.put("sys_cfg",data(152,1)));
    ok(s.put("proto_a",data(3010,2)));
    ok(s.put("proto_b",data(3010,3)));
    ok(s.put("dev_text",data(4000,4)));
    ok(s.put("web_sysinfo",data(256,5)));
    ok(s.put("do_bits",data(8,6)));
    ok(s.put("dev_cfg",data(1024,7)));
    for(int i=0;i<24;i++) { auto name="scalar"+std::to_string(i); ok(s.storage.writeItem(s.ns,name.c_str(),uint32_t(i))); }
}
static void repeated_saves() {
    for(bool erase_first:{true,false}) {
        Nor n; Store s(n); populate(s); n.counters();
        for(int i=0;i<50;i++) {
            ok(s.put("sys_cfg",data(152,1),erase_first));
            ok(s.put("web_sysinfo",data(256,5),erase_first));
        }
        std::printf("same_value mode=%s writes=%zu erases=%zu\n",erase_first?"delete-before-set":"replace",n.writes,n.erases);
        if(!erase_first) require(n.writes==0 && n.erases==0,"identical replacement wrote Flash");
    }
}
static void full_records_gc() {
    Nor n;
    {
        Store s(n); populate(s); n.counters();
        for(int round=0;round<200;round++) {
            auto v=uint8_t(round);
            ok(s.put("sys_cfg",data(152,v)));
            ok(s.put("proto_a",data(3010,v)));
            ok(s.put("proto_b",data(3010,uint8_t(v+1))));
            ok(s.put("dev_text",data(4000,v)));
            ok(s.put("web_sysinfo",data(256,v)));
            ok(s.put("do_bits",data(8,v)));
            ok(s.put("dev_cfg",data(1024,v)));
            require(s.get("dev_text",4000)==data(4000,v),"text mismatch after GC");
            require(s.get("proto_a",3010)==data(3010,v),"proto mismatch after GC");
        }
    }
    Store recovered(n);
    require(recovered.get("dev_text",4000)==data(4000,199),"reboot text mismatch");
    require(recovered.get("sys_cfg",152)==data(152,199),"reboot cfg mismatch");
    std::printf("24KB mixed records rounds=200 writes=%zu erases=%zu recovery=PASS\n",n.writes,n.erases);
}
static void cut_each_operation() {
    Nor initial; { Store s(initial); populate(s); }
    struct Record { const char* key; size_t size; uint8_t original; };
    const Record records[]={{"sys_cfg",152,1},{"proto_a",3010,2},{"proto_b",3010,3},
        {"dev_text",4000,4},{"web_sysinfo",256,5},{"do_bits",8,6},{"dev_cfg",1024,7}};
    size_t checked=0;
    for(const auto& record:records) {
        Nor probe; probe.bytes=initial.bytes;
        {
            Store s(probe); probe.counters(); probe.cut_after=0x7fffffff;
            ok(s.put(record.key,data(record.size,0x6d)));
        }
        size_t operation_count=probe.operations;
        require(operation_count>0,"replacement did not exercise partition operations");
        for(size_t cut=0;cut<operation_count;cut++) {
            Nor n; n.bytes=initial.bytes;
            {
                Store s(n); n.counters(); n.cut_after=cut;
                require(s.put(record.key,data(record.size,0x6d))!=ESP_OK,"injected cut was not observed");
            }
            n.cut_after=-1;
            Store recovered(n);
            auto value=recovered.get(record.key,record.size);
            require(value==data(record.size,record.original) || value==data(record.size,0x6d),
                "partial/missing blob after power cut");
            for(const auto& other:records) {
                if(std::strcmp(other.key,record.key)!=0)
                    require(recovered.get(other.key,other.size)==data(other.size,other.original),
                        "unrelated blob damaged by cut");
            }
            checked++;
        }
        std::printf("cut_scan key=%s actual_operations=%zu recovery=PASS\n",record.key,operation_count);
    }
    std::printf("24KB replacements actual_injected_cuts=%zu old_or_new=PASS all_other_blobs=PASS\n",checked);
}
static void exhausted_partition_preserves_values() {
    Nor n;
    {
        Store s(n); populate(s);
        int count=0;
        for(;count<1000;count++) {
            auto key="fill"+std::to_string(count);
            auto error=s.put(key.c_str(),data(128,0x81));
            if(error==ESP_ERR_NVS_NOT_ENOUGH_SPACE) break;
            ok(error);
        }
        require(count<1000,"failed to exercise partition exhaustion");
        auto error=s.put("dev_text",data(4000,0xa9));
        require(error==ESP_ERR_NVS_NOT_ENOUGH_SPACE,"full partition did not reject oversized replacement");
        require(s.get("dev_text",4000)==data(4000,4),"space failure deleted previous text");
    }
    Store recovered(n);
    require(recovered.get("dev_text",4000)==data(4000,4),"reboot lost text after space failure");
    require(recovered.get("sys_cfg",152)==data(152,1),"space failure damaged unrelated cfg");
    std::printf("24KB exhausted replacement old_text=PASS unrelated_cfg=PASS reboot=PASS\n");
}
int main() {
    try { repeated_saves(); full_records_gc(); cut_each_operation(); exhausted_partition_preserves_values(); }
    catch(const std::exception& e) { std::fprintf(stderr,"FAIL: %s\n",e.what()); return 1; }
    return 0;
}
'''


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--idf-path", type=Path,
                        default=Path(os.environ.get("IDF_PATH", ROOT.parent / "esp-idf")))
    args = parser.parse_args()
    idf = args.idf_path.resolve()
    nvs = idf / "components/nvs_flash"
    with tempfile.TemporaryDirectory(prefix="gateway-nvs-engine-") as temporary:
        root = Path(temporary)
        stubs = {
            "sdkconfig.h": "#pragma once\n",
            "esp_attr.h": "#pragma once\n#define IDF_DEPRECATED(msg)\n",
            "esp_heap_caps.h": "#pragma once\n",
            "esp_partition.h": "#pragma once\n",
            "spi_flash_mmap.h": "#pragma once\n#define ESP_ERR_FLASH_OP_FAIL 0x6001\n",
            "esp_rom_caps.h": "#define ESP_ROM_HAS_CRC_BE 1\n#define ESP_ROM_HAS_CRC_LE 0\n",
            "esp_rom_crc.h": "#pragma once\n#include <stdint.h>\nextern \"C\" uint32_t esp_rom_crc32_le(uint32_t,const uint8_t*,uint32_t);\n",
            "esp_log.h": "#pragma once\n" + "".join(f"#define ESP_LOG{x}(...) ((void)0)\n" for x in "EWDIV"),
        }
        for name, body in stubs.items():
            (root / name).write_text(body)
        (root / "test.cpp").write_text(HARNESS)
        includes = [root, nvs / "src", nvs / "include", nvs / "private_include",
                    idf / "components/esp_common/include"]
        flags = [arg for path in includes for arg in ("-I", str(path))]
        subprocess.run(["cc", "-c", *flags,
                        str(idf / "components/esp_rom/patches/esp_rom_crc.c"),
                        "-o", str(root / "crc.o")], check=True)
        sources = ["nvs_storage.cpp", "nvs_page.cpp", "nvs_pagemanager.cpp",
                   "nvs_item_hash_list.cpp", "nvs_types.cpp"]
        subprocess.run(["c++", "-std=c++17", "-O2", "-DNO_DEBUG_STORAGE", *flags,
                        str(root / "test.cpp"), *[str(nvs / "src" / s) for s in sources],
                        str(root / "crc.o"), "-o", str(root / "tests")], check=True)
        subprocess.run([str(root / "tests")], check=True)


if __name__ == "__main__":
    main()
