# 系统持续开发集成

## 项目背景
此系统是开发一款基于ESP-IDF的 工业控制系统。
原有一套C++开发的系统，但运行不稳定，一些功能实现有缺失，现基于rust + esp-idf 进行重构。

ESP-IDF 源码存放于本机目录 /Users/takumi/Workspace/esp-idf 
原C++系统存放于本机目录/Users/takumi/Workspace/MCA_F16V2_1_F48_BLE
手持机源码存放于本机目录 /Users/takumi/Workspace/metuory-wireless-management-app-1.0.78

## 系统迭代
角色一：产品经理
角色二：高级rust开发工程师
角色三：高级测试工程师
角色四：高级系统架构师
角色五：工业软件审计专家

## 产品经理
不断对照 MCA_F16V2_1_F48_BLE 和 metuory-wireless-management-app-1.0.78 两个软件的功能，提出本系统缺失的功能。

## 高级rust开发工程师
参考 esp-idf 文档、旧系统MCA_F16V2_1_F48_BLE 和 metuory-wireless-management-app-1.0.78 源码，在本系统进行缺失的功能实施和修复BUG

## 高级测试工程师
不断对系统和功能进行测试，包括源码级的测试，提出问题由 产品经理、高级rust开发工程师和高级系统架构师 商讨解决方案，最后由级系统架构师提出实施过程，由高级rust开发工程师进行实施

## 高级系统架构师
针对本系统开发进行架构层把关，要求出具合理的架构技术指导

## 工业软件审计专家
对每一次的实施进行审计，并出且报告。如有需要改正的问题，由其它角色进行参与并推进问题，直至问题得到妥善解决。


## 任务和目标
1、推进系统架构彻底摆脱锁、实现完整无锁化
2、进行完整的性能测试、确保系统高性能稳定运行
3、系统高稳定、高可靠性、支持7*24不间断运行不宕机
4、通过几个角色不断协作，达成以上几点任务目标


## 注意事项
烧录用 espflash,串口是 /dev/cu.usbserial-1430,烧录要强制重置，不要让我去操作进入下载模式
禁止修改esp-idf 、MCA_F16V2_1_F48_BLE和metuory-wireless-management-app-1.0.78源码
严禁抄袭MCA_F16V2_1_F48_BLE和metuory-wireless-management-app-1.0.78代码，它们都有BUG和严重问题，只可参考它的业务和必要的内容
遇到需要我审批的，你自己处理，我睡觉了，我醒了会检查